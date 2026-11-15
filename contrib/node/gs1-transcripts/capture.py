#!/usr/bin/env python3
"""Record GS1 governance-signer transcripts from a release dashd 24.0.0 masternode.

Run with python3 -I. This extends the GV1b recorder (contrib/node/gv1b-transcripts):
its node, transcript, peer and record grammar are reused, and the frozen GV1b
bootstrap is replayed first. --build-setup extends that chain once with two
masternode registrations whose keys are derived from fixed labels and the
proposal collaterals; the result is frozen in test-vectors/gs1/G.setup.json.gz.
Collaterals and fees spend the bootstrap's coinbase outputs to Core's public
test-framework address; its key is read at run time from the --core tree at
v24.0.0 and is held in memory only. Each case then starts a release dashd that
runs as the recording masternode (operator key from the label gs1-operator-0)
and records what its governance signer sends. --check re-captures G1 and
compares it with the stored transcript. --selftest parses every fixture in
test-vectors/gs1. Datadirs are confined to /work/scratch/gs1-0/captures, with
mode 0700.
"""

import argparse
import contextlib
import difflib
import gzip
import hashlib
import importlib.util
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
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
ROOT = Path(os.environ.get('GS1_CAPTURE_DIR', '/work/scratch/gs1-0/captures'))
FIXTURES = REPO / 'test-vectors/gs1'
GV1B = REPO / 'dashd-governance/tests/fixtures/dashd-v24.0.0'
SETUP = 'G.setup.json.gz'


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


gv = load('gs1_gv1b', REPO / 'contrib/node/gv1b-transcripts/capture.py')
p2a = gv.p2a
gv.ROOT = ROOT
p2a.ROOT = ROOT
DASHD = gv.DASHD
SIM_START = gv.SIM_START
CHAIN_ARGS = gv.CHAIN_ARGS
encoded = gv.encoded
hbytes = gv.hbytes

# The recording masternode's P2P port is its ProTx service, so it is fixed:
# a probed port would change the registration and every later hash.
MN_PORT = 29440
SECOND_PORT = 29442
RECORDER = 'gs1-operator-0'
SECOND = 'gs1-operator-1'
REPLACEMENT = 'gs1-operator-2'
COIN = 100_000_000
FEE = 1_000_000
CYCLE = 20
WINDOW = 10
WINDOW_TIME_OFFSET = 60
GOVERNANCE_UPDATE_MIN = 3600
IBD_OFFSET = 3 * 86400
FUNDING, YES, NO = 1, 1, 2
RELAY_QUIET = 1.5
PEER_STEP = 1199
CASES = ['G1', 'G2', 'G3', 'G4', 'G5', 'G6', 'G6-ibd', 'G7', 'G8', 'G9', 'G10', 'G11', 'G12']

# ---- keys derived from fixed labels (Q4); secrets exist only in memory

BLS_ORDER = 0x73EDA753299D7D483339D80809A1D80553BDA402FFFE5BFEFFFFFFFF00000001
EC_ORDER = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
EC_P = 2**256 - 2**32 - 977
EC_G = (0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
        0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8)


def sha256(raw):
    return bytes.fromhex(hashlib.sha256(raw).hexdigest())


def sha256d(raw):
    return sha256(sha256(raw))


def hmac_sha256(key, msg):
    import hmac
    return bytes.fromhex(hmac.new(key, msg, 'sha256').hexdigest())


def label_scalar(label, order):
    value = int(hashlib.sha256(label.encode()).hexdigest(), 16) % order
    if not value:
        raise ValueError(f'label {label} reduces to zero')
    return value


def bls_public(label):
    from py_ecc.bls import G2Basic
    return G2Basic.SkToPk(label_scalar(label, BLS_ORDER))


def bls_sign(label, msg32):
    from py_ecc.bls import G2Basic
    return G2Basic.Sign(label_scalar(label, BLS_ORDER), msg32)


def bls_argument(label):
    return '%064x' % label_scalar(label, BLS_ORDER)


def ec_add(a, b):
    if a is None:
        return b
    if b is None:
        return a
    if a[0] == b[0] and (a[1] + b[1]) % EC_P == 0:
        return None
    if a == b:
        slope = 3 * a[0] * a[0] * pow(2 * a[1], -1, EC_P)
    else:
        slope = (b[1] - a[1]) * pow(b[0] - a[0], -1, EC_P)
    x = (slope * slope - a[0] - b[0]) % EC_P
    return x, (slope * (a[0] - x) - a[1]) % EC_P


def ec_mul(k, point=EC_G):
    result = None
    while k:
        if k & 1:
            result = ec_add(result, point)
        point = ec_add(point, point)
        k >>= 1
    return result


def ec_point_bytes(scalar):
    x, y = ec_mul(scalar)
    return bytes([2 + (y & 1)]) + x.to_bytes(32, 'big')


def ec_public(label):
    return ec_point_bytes(label_scalar(label, EC_ORDER))


def hash160(raw):
    return bytes.fromhex(hashlib.new('ripemd160', sha256(raw)).hexdigest())


def key_id(label):
    return hash160(ec_public(label))


def p2pkh_script(kid):
    return b'\x76\xa9\x14' + kid + b'\x88\xac'


def p2pkh(label):
    return p2pkh_script(key_id(label))


B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'


def b58check(raw):
    raw += sha256d(raw)[:4]
    n = int.from_bytes(raw, 'big')
    text = ''
    while n:
        n, r = divmod(n, 58)
        text = B58[r] + text
    return '1' * (len(raw) - len(raw.lstrip(b'\0'))) + text


def b58check_decode(text):
    n = 0
    for c in text:
        if c not in B58:
            raise ValueError('base58 alphabet')
        n = n * 58 + B58.index(c)
    raw = n.to_bytes((n.bit_length() + 7) // 8, 'big')
    raw = bytes(len(text) - len(text.lstrip('1'))) + raw
    if len(raw) < 5 or sha256d(raw[:-4])[:4] != raw[-4:]:
        raise ValueError('base58 checksum')
    return raw[:-4]


def address_of(kid):
    return b58check(b'\x8c' + kid)


def address(label):
    return address_of(key_id(label))


def ecdsa_sign(secret, msg32):
    """RFC6979 nonce, low s; returns (r, s, recovery id)."""
    z = int.from_bytes(msg32, 'big')
    v, k = b'\x01' * 32, b'\x00' * 32
    seed = secret.to_bytes(32, 'big') + msg32
    k = hmac_sha256(k, v + b'\x00' + seed)
    v = hmac_sha256(k, v)
    k = hmac_sha256(k, v + b'\x01' + seed)
    v = hmac_sha256(k, v)
    while True:
        v = hmac_sha256(k, v)
        nonce = int.from_bytes(v, 'big')
        if 0 < nonce < EC_ORDER:
            point = ec_mul(nonce)
            r = point[0] % EC_ORDER
            s = pow(nonce, -1, EC_ORDER) * (z + r * secret) % EC_ORDER
            if r and s:
                break
        k = hmac_sha256(k, v + b'\x00')
        v = hmac_sha256(k, v)
    recid = (point[1] & 1) | (2 if point[0] >= EC_ORDER else 0)
    if s > EC_ORDER // 2:
        s = EC_ORDER - s
        recid ^= 1
    return r, s, recid


def sign_compact(label, msg32):
    """CKey::SignCompact equivalent: low-s, RFC6979 nonce, recovery header for a compressed key."""
    r, s, recid = ecdsa_sign(label_scalar(label, EC_ORDER), msg32)
    return bytes([27 + 4 + recid]) + r.to_bytes(32, 'big') + s.to_bytes(32, 'big')


def der_signature(r, s):
    def integer(x):
        raw = x.to_bytes(33, 'big').lstrip(b'\0')
        if raw[0] & 0x80:
            raw = b'\0' + raw
        return b'\x02' + bytes([len(raw)]) + raw
    body = integer(r) + integer(s)
    return b'\x30' + bytes([len(body)]) + body


# ---- funding (DEC-1719): Core's public test-framework key, read at run time and held in memory only

FUNDING_ADDRESS = 'yYdShjQSptFKitYLksFEUSwHe4hnbar5rf'
FUNDING_SOURCE = 'test/functional/test_framework/test_node.py'
FUNDING_LINE = 185
FUNDING_TAG = 'v24.0.0'


class P2pkhSigner:
    """Signs P2PKH inputs; the scalar never leaves this object and nothing formats it."""

    __slots__ = ('_secret', 'public', 'script')

    def __init__(self, secret):
        self._secret = secret
        self.public = ec_point_bytes(secret)
        self.script = p2pkh_script(hash160(self.public))

    def __repr__(self):
        return '<p2pkh signer>'

    @classmethod
    def from_label(cls, label):
        return cls(label_scalar(label, EC_ORDER))

    def sign(self, msg32):
        r, s, _ = ecdsa_sign(self._secret, msg32)
        return der_signature(r, s) + b'\x01'


def funding_entry(core):
    """The WIF of PRIV_KEYS[0] at FUNDING_SOURCE:FUNDING_LINE, Core v24.0.0, or None. Never logged."""
    try:
        source = subprocess.run(['git', '-C', str(core), 'show', f'{FUNDING_TAG}:{FUNDING_SOURCE}'],
                                capture_output=True, check=True).stdout.decode().splitlines()
        first = next(i for i, line in enumerate(source) if line.strip() == 'PRIV_KEYS = [')
        entry = next(i for i in range(first + 1, len(source)) if 'AddressKeyPair(' in source[i])
        match = re.fullmatch(r"\s*AddressKeyPair\('([1-9A-HJ-NP-Za-km-z]{34})', '([1-9A-HJ-NP-Za-km-z]{52})'\),?\s*",
                             source[entry])
        if entry + 1 == FUNDING_LINE and match and match[1] == FUNDING_ADDRESS:
            return match[2]
    except Exception:  # noqa: BLE001 - every failure is reported the same way, with no text from the file
        pass
    return None


def funding_secret(wif):
    """The scalar of a compressed testnet WIF that derives FUNDING_ADDRESS, or None."""
    try:
        raw = b58check_decode(wif)
    except ValueError:
        return None
    if len(raw) != 34 or raw[0] != 0xef or raw[33] != 1:
        return None
    secret = int.from_bytes(raw[1:33], 'big')
    if not 0 < secret < EC_ORDER or address_of(hash160(ec_point_bytes(secret))) != FUNDING_ADDRESS:
        return None
    return secret


def funding_key(core):
    """PRIV_KEYS[0] of Core's test framework at v24.0.0; exits without echoing anything it read."""
    wif = funding_entry(core)
    secret = funding_secret(wif) if wif is not None else None
    del wif
    if secret is None:
        print(f'funding key: {FUNDING_SOURCE}:{FUNDING_LINE} at {FUNDING_TAG} under --core is missing '
              f'or does not derive {FUNDING_ADDRESS}', file=sys.stderr)
        raise SystemExit(2)
    print(f'funding key: derived {FUNDING_ADDRESS} from {FUNDING_SOURCE}:{FUNDING_LINE} at {FUNDING_TAG}')
    return P2pkhSigner(secret)


# ---- serialization


def varbytes(raw):
    return p2a.compact(len(raw)) + raw


def outpoint(txid, n):
    return bytes.fromhex(txid)[::-1] + struct.pack('<I', n)


def service(port):
    return bytes(10) + b'\xff\xff' + bytes([127, 0, 0, 1]) + struct.pack('>H', port)


def transaction(inputs, outputs, tx_type, payload, signer):
    """A version 3 transaction whose inputs are all P2PKH outputs of `signer`, each signed SIGHASH_ALL
    over the legacy serialization, which carries the special payload (interpreter.cpp:1434)."""

    def serialize(script_sigs, sighash_type=None):
        raw = struct.pack('<I', 3 | (tx_type << 16)) + p2a.compact(len(inputs))
        for (txid_, n, _), script in zip(inputs, script_sigs, strict=True):
            raw += outpoint(txid_, n) + varbytes(script) + b'\xff\xff\xff\xff'
        raw += p2a.compact(len(outputs))
        for value, script in outputs:
            raw += struct.pack('<q', value) + varbytes(script)
        raw += struct.pack('<I', 0)
        if tx_type:
            raw += varbytes(payload)
        if sighash_type is not None:
            raw += struct.pack('<I', sighash_type)
        return raw

    script_sigs = []
    for index in range(len(inputs)):
        codes = [signer.script if i == index else b'' for i in range(len(inputs))]
        sig = signer.sign(sha256d(serialize(codes, 1)))
        script_sigs.append(varbytes(sig) + varbytes(signer.public))
    return serialize(script_sigs)


def inputs_hash(inputs):
    return sha256d(b''.join(outpoint(txid, n) for txid, n, _ in inputs))


def txid(raw):
    return sha256d(raw)[::-1].hex()


def pro_reg_tx(inputs, operator, owner, voting, payout, port, change, signer):
    """CProRegTx version 2 (BasicBLS) with an internal collateral at output 0."""
    total = sum(v for _, _, v in inputs)
    outputs = [(1000 * COIN, p2pkh(change.replace('change', 'collateral'))),
               (total - 1000 * COIN - FEE, p2pkh(change))]
    payload = (struct.pack('<HHH', 2, 0, 0) + bytes(32) + struct.pack('<I', 0) + service(port)
               + key_id(owner) + bls_public(operator) + key_id(voting) + struct.pack('<H', 0)
               + varbytes(p2pkh(payout)) + inputs_hash(inputs) + varbytes(b''))
    return transaction(inputs, outputs, 1, payload, signer)


def pro_up_reg_tx(inputs, pro_tx_hash, operator, owner, voting, payout, change, signer):
    """CProUpRegTx version 2, signed by the owner key over its SER_GETHASH serialization."""
    total = sum(v for _, _, v in inputs)
    unsigned = (struct.pack('<H', 2) + bytes.fromhex(pro_tx_hash)[::-1] + struct.pack('<H', 0)
                + bls_public(operator) + key_id(voting) + varbytes(p2pkh(payout)) + inputs_hash(inputs))
    payload = unsigned + varbytes(sign_compact(owner, sha256d(unsigned)))
    return transaction(inputs, [(total - FEE, p2pkh(change))], 3, payload, signer)


def govobj(time_, data, kind, mn_outpoint=None, collateral=bytes(32), signer=None):
    """CGovernanceObject: wire bytes and Object::GetHash (common.cpp:23-39)."""
    op = mn_outpoint if mn_outpoint is not None else bytes(32) + b'\xff\xff\xff\xff'
    head = bytes(32) + struct.pack('<iq', 1, time_)
    unsigned = head + collateral + varbytes(data) + struct.pack('<i', kind) + op
    sig = bls_sign(signer, sha256d(unsigned)) if signer else b''
    h = sha256d(head + varbytes(data.hex().encode()) + op + b'\x00\xff\xff\xff\xff' + varbytes(sig))
    return h[::-1].hex(), unsigned + varbytes(sig)


def vote(mn_outpoint, parent, outcome, time_, ecdsa=None, operator=None):
    """CGovernanceVote (vote.h): FUNDING signal; voting-key ECDSA or operator BLS signature.

    Outside testnet a voting-key signature is a signed message over GetSignatureString()
    (vote.cpp:175-186, vote.h:166-172, messagesigner.cpp:39-46); the operator signs the hash."""
    body = mn_outpoint + bytes.fromhex(parent)[::-1] + struct.pack('<ii', outcome, FUNDING) + struct.pack('<q', time_)
    if ecdsa:
        n, = struct.unpack('<I', mn_outpoint[32:])
        text = f'{mn_outpoint[:32][::-1].hex()}-{n}|{parent}|{FUNDING}|{outcome}|{time_}'
        sig = sign_compact(ecdsa, sha256d(varbytes(b'DarkCoin Signed Message:\n') + varbytes(text.encode())))
    else:
        sig = bls_sign(operator, sha256d(body))
    h = sha256d(mn_outpoint + b'\x00\xff\xff\xff\xff' + bytes.fromhex(parent)[::-1]
                + struct.pack('<iiq', FUNDING, outcome, time_))
    return h[::-1].hex(), body + varbytes(sig)


def amount_text(sat):
    return f'{sat // COIN}.{sat % COIN:08d}'


def trigger_data(height, payments):
    """CSuperblock::GetHexStrData (superblock.cpp:389-413) as raw bytes; payments in output order."""
    text = ('{"event_block_height": %d, "payment_addresses": "%s", "payment_amounts": "%s", '
            '"proposal_hashes": "%s", "type":2}') % (
        height, '|'.join(p['address'] for p in payments),
        '|'.join(amount_text(p['amount']) for p in payments), '|'.join(p['hash'] for p in payments))
    return text.encode()


def proposal_data(name, start, end, amount, payee):
    return ('{"end_epoch":%d,"name":"%s","payment_address":"%s","payment_amount":%s,'
            '"start_epoch":%d,"type":1,"url":"https://dash.org"}') % (
        end, name, address(payee), amount_text(amount), start)


def parse_govobj(raw):
    """(type, masternode outpoint) of a wire govobj."""
    stream = __import__('io').BytesIO(raw)
    gv.take(stream, 32 + 4 + 8 + 32)
    gv.take(stream, gv.read_compact(stream))
    kind, = struct.unpack('<i', gv.take(stream, 4))
    return kind, gv.take(stream, 36)


# ---- the frozen setup chain


def funding_coins(rpc, funder):
    """Unspent, mature bootstrap coinbase outputs (blocks 1-179) paid to the funding address."""
    tip = rpc('getblockcount')
    coins = []
    for height in range(1, min(179, tip - 99) + 1):
        tx = rpc('getblock', rpc('getblockhash', height), 2)['tx'][0]
        for o in tx['vout']:
            if bytes.fromhex(o['scriptPubKey']['hex']) == funder.script and rpc('gettxout', tx['txid'], o['n']):
                coins.append((tx['txid'], o['n'], o['valueSat']))
    return coins


def proposal_hash(spec, time_):
    name, start, end, value, payee = spec
    return govobj(time_, proposal_data(name, start, end, value, payee).encode(), 1)[0]


def memcmp_key(h):
    """uint256::operator< compares the stored (little-endian) bytes with memcmp."""
    return bytes.fromhex(h)[::-1]


def g5_names(specs, time_):
    """Suffix the E pair and C names until G5 separates the orderings it pins: the equal-yes E pair
    sorts differently under arith (UintToArith256) and memcmp comparison, and the arith winner's hash
    is above C's, so the final hash sort (signing.cpp:113-115) reverses C (two yes votes, filled
    first) and that winner."""
    def renamed(key, n):
        name, *rest = specs[key]
        return (f'{name}_{n}', *rest)

    for n in range(10_000):
        e1, e2 = (proposal_hash(renamed(k, n), time_) for k in ('E1', 'E2'))
        if (int(e1, 16) > int(e2, 16)) != (memcmp_key(e1) > memcmp_key(e2)):
            specs = dict(specs, E1=renamed('E1', n), E2=renamed('E2', n))
            winner = max(int(e1, 16), int(e2, 16))
            break
    else:
        raise RuntimeError('no E pair name separates arith and memcmp order')
    for m in range(10_000):
        if int(proposal_hash(renamed('C', m), time_), 16) < winner:
            return dict(specs, C=renamed('C', m))
    raise RuntimeError('no C name sorts below the E pair winner')


def build_setup(data, out, funder):
    """Extend the bootstrap: two registrations and the proposal collaterals, funded by the bootstrap's
    coinbase outputs to the funding address; every new block pays its coinbase to OP_TRUE.

    Generation is deterministic under mock time; the blocks are stored so replays need no miner.
    """
    with observed_node('setup', data, port=None) as node:
        rpc = node.rpc
        base_height = rpc('getblockcount')
        mature = funding_coins(rpc, funder)

        def take(value):
            picked = []
            while sum(c[2] for c in picked) < value:
                picked.append(mature.pop(0))
            return picked

        operators = {}
        for index, label in enumerate((RECORDER, SECOND)):
            port = MN_PORT if label == RECORDER else SECOND_PORT
            raw = pro_reg_tx(take(1000 * COIN + FEE + COIN), label, f'gs1-owner-{index}', f'gs1-voting-{index}',
                             f'gs1-payout-{index}', port, f'gs1-change-{index}', funder)
            h = rpc('sendrawtransaction', raw.hex())
            operators[label] = {'proTxHash': h, 'collateral': [h, 0], 'owner': f'gs1-owner-{index}',
                                'voting': f'gs1-voting-{index}', 'payout': f'gs1-payout-{index}', 'port': port}
        rpc('generatetodescriptor', 1, 'raw(51)')
        # The G6/G10 ProUpRegTx fee spends the recording masternode's registration change output.
        change = rpc('getrawtransaction', operators[RECORDER]['proTxHash'], True)['vout'][1]
        if bytes.fromhex(change['scriptPubKey']['hex']) != p2pkh('gs1-change-0'):
            raise RuntimeError('registration change output is not the gs1-change-0 key')
        spare = {'outpoint': [operators[RECORDER]['proTxHash'], 1], 'value': change['valueSat'],
                 'label': 'gs1-change-0'}
        for label, info in operators.items():
            listed = rpc('protx', 'info', info['proTxHash'])
            if listed['state']['pubKeyOperator'] != bls_public(label).hex():
                raise RuntimeError('registration did not take the derived operator key')
        # The first window tip h1 = S + 1 needs S % 20 == 9 and the recording masternode as the first
        # projected payee there, with every proposal collateral six blocks deep by S.
        now = rpc('getblock', rpc('getbestblockhash'))['time'] + 120
        rpc('setmocktime', now)
        mine = address(operators[RECORDER]['payout'])

        def leading(tip):
            return rpc('masternode', 'winners').get(str(tip + 2), '').startswith(mine)

        planned = rpc('getblockcount') + 6
        planned += (WINDOW - 1 - planned) % CYCLE
        budget = round(rpc('getsuperblockbudget', planned + 1 + WINDOW) * COIN)
        if budget < 2 * COIN:
            raise RuntimeError(f'superblock budget too small: {budget}')
        window_time = now + WINDOW_TIME_OFFSET
        edge = window_time + 1572 - 7200
        half = budget // 2 + 5_000_000
        later = now + 30 * 86400
        specs = {
            'A': ('GS1_A', now - 3600, later, COIN, 'gs1-payee-a'),
            'B': ('GS1_B_over', now - 3600, later, budget + COIN, 'gs1-payee-b'),
            'C': ('GS1_C_edge_in', edge - 86400, edge, 30_000_000, 'gs1-payee-c'),
            'D': ('GS1_D_edge_out', edge - 1 - 86400, edge - 1, 20_000_000, 'gs1-payee-d'),
            'E1': ('GS1_E_pair_1', now - 3600, later, half, 'gs1-payee-e1'),
            'E2': ('GS1_E_pair_2', now - 3600, later, half, 'gs1-payee-e2'),
        }
        specs = g5_names(specs, now - 600)
        proposals = {}
        for key, (name, start, end, value, payee) in specs.items():
            text = proposal_data(name, start, end, value, payee).encode()
            h, _ = govobj(now - 600, text, 1)
            inputs = take(COIN + FEE)
            total = sum(v for _, _, v in inputs)
            raw = transaction(inputs, [(COIN, b'\x6a\x20' + bytes.fromhex(h)[::-1]),
                                       (total - COIN - FEE, p2pkh('gs1-change-proposals'))], 0, b'', funder)
            collateral = rpc('sendrawtransaction', raw.hex())
            h2, obj = govobj(now - 600, text, 1, collateral=bytes.fromhex(collateral)[::-1])
            assert h2 == h
            proposals[key] = {'hash': h, 'hex': obj.hex(), 'data': text.decode(), 'collateral': collateral,
                              'amount': value, 'address': address(payee), 'time': now - 600}
        e1, e2 = proposals['E1']['hash'], proposals['E2']['hash']
        print(f"G5: E1 {e1}, E2 {e2}; arith picks {max(e1, e2, key=lambda h: int(h, 16))}, "
              f"memcmp picks {max(e1, e2, key=memcmp_key)}; C {proposals['C']['hash']}")
        rpc('generatetodescriptor', 6, 'raw(51)')
        for _ in range(1000):
            target = rpc('getblockcount')
            if target % CYCLE == WINDOW - 1 and leading(target):
                break
            if rpc('protx', 'info', operators[RECORDER]['proTxHash'])['state']['PoSeBanHeight'] != -1:
                raise RuntimeError('the recording masternode was PoSe-banned during setup')
            rpc('generatetodescriptor', 1, 'raw(51)')
        else:
            raise RuntimeError('the recording masternode never led at a first window tip')
        superblock = target + 1 + WINDOW
        actual = round(rpc('getsuperblockbudget', superblock) * COIN)
        amounts = {k: p['amount'] for k, p in proposals.items()}
        if not (amounts['B'] > actual and amounts['E1'] + amounts['E2'] > actual
                and amounts['E1'] + amounts['C'] <= actual and amounts['A'] <= actual):
            raise RuntimeError(f'superblock budget {actual} breaks the G5 amounts {amounts}')
        budget = actual
        for p in proposals.values():
            if rpc('getrawtransaction', p['collateral'], True)['confirmations'] < 6:
                raise RuntimeError('proposal collateral has fewer than six confirmations at S')
        if rpc('gettxout', *spare['outpoint']) is None:
            raise RuntimeError('the ProUpRegTx fee output was spent during setup')
        blocks = [rpc('getblock', rpc('getblockhash', h), 0) for h in range(base_height + 1, target + 1)]
        setup = {'blocks': blocks, 'time': now, 'window_time': window_time, 'tip': target,
                 'superblock': superblock, 'budget': budget, 'operators': operators,
                 'proposals': proposals, 'spare': spare, 'base': base_height}
        with gzip.GzipFile(out / SETUP, 'wb', mtime=0) as f:
            f.write((encoded(setup) + '\n').encode())
        print(f'setup: {len(blocks)} blocks to S={target}, superblock {superblock}, budget {budget}')


# ---- nodes


def probe_port(port):
    with socket.socket() as s:
        # As dashd binds: a TIME_WAIT left by the previous run's RPC client does not hold the port.
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            s.bind(('127.0.0.1', port))
        except OSError:
            return False
    return True


@contextlib.contextmanager
def observed_node(name, data, port=MN_PORT, setup=None, extra=()):
    """A fresh release node fed the bootstrap (and setup) blocks before its transcript starts."""
    if port is None:
        p2a.PORT_BASE = gv.free_ports()
    else:
        if not (probe_port(port) and probe_port(port + 1)):
            raise RuntimeError(f'fixed masternode ports {port}/{port + 1} are busy')
        p2a.PORT_BASE = port
    ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    run_root = Path(tempfile.mkdtemp(prefix=f'{name}-', dir=ROOT))
    previous = p2a.ROOT
    p2a.ROOT = run_root
    node = None
    try:
        args = list(CHAIN_ARGS) + ['-disablewallet', *extra]
        if setup is not None:
            args += [f'-masternodeblsprivkey={bls_argument(RECORDER)}', '-externalip=127.0.0.1']
        node = gv.Node(name, 0, SIM_START, extra=args)
        node.rpc('setmocktime', data['time'])
        blocks = data['blocks'][:data['tip']] + (setup['blocks'] if setup else [])
        for block in blocks:
            answer = node.rpc('submitblock', block)
            if answer is not None:
                raise RuntimeError(f'replayed block rejected: {answer}')
        if setup is not None:
            node.rpc('setmocktime', setup['time'])
        yield node
    finally:
        if node is not None:
            node.stop()
        p2a.ROOT = previous
        shutil.rmtree(run_root)


class Transcript(gv.Transcript):
    def __init__(self, name, node, now, setup):
        super().__init__(name, node, now)
        self.lines[1] = re.sub(r'-masternodeblsprivkey=[0-9a-f]+', f'-masternodeblsprivkey=<{RECORDER}>', self.lines[1])
        if re.search(r'[0-9a-f]{64}', self.lines[1]):
            raise RuntimeError('a 64-hex value reached the args line')
        self.lines[2] = f'# bootstrap: bootstrap.json.gz, then {SETUP}; blocks in height order'
        info = setup['operators'][RECORDER]
        self.lines.insert(3, f'operator {RECORDER} {info["proTxHash"]} {info["collateral"][0]}:{info["collateral"][1]}')
        self.setup = setup
        self.mn_outpoint = outpoint(*info['collateral'])
        self.scan = len(self.lines)
        self.fetched = set()

    def logs(self):
        with (self.node.dir / 'regtest/debug.log').open('rb') as f:
            f.seek(self.log_offset)
            complete, _, _ = f.read().rpartition(b'\n')
            if not complete:
                return
            self.log_offset += len(complete) + 1
            lines = complete.decode(errors='strict').splitlines()
        keep = ('Misbehaving:', 'Rejected vote', 'MNGOVERNANCEOBJECT --',
                'CreateSuperblock', 'CreateGovernanceTrigger --', 'VoteGovernanceTriggers --',
                'VoteFundingTrigger --', 'CActiveMasternodeManager::',
                'CGovernanceManager::UpdatedBlockTip -- nCachedBlockHeight:',
                'MasternodeRateCheck --', 'CGovernanceManager::AddGovernanceObject --',
                'CSuperblock::IsExpired', 'UpdateCachesAndClean -- erase obj')
        for line in lines:
            match = re.search(r'Misbehaving: peer=(\d+) \(\d+ -> (\d+)\)', line)
            if match:
                for n, core_id in self.peer_ids.items():
                    if core_id == int(match[1]):
                        self.scores[n] = int(match[2])
            if any(s in line for s in keep):
                self.observed_logs.append('log ' + encoded(line))

    def checkpoint(self, label):
        super().checkpoint(label)
        self.rpc_record('masternode', 'status', allow_error=True)
        self.rpc_record('getgovernanceinfo')

    def collect(self, n=1):
        """Fetch every governance inventory the node announced to peer n; mark the signer's own."""
        wanted = []
        for line in self.lines[self.scan:]:
            fields = line.split()
            if len(fields) == 4 and fields[:3] == ['<', str(n), 'inv']:
                raw = bytes.fromhex(fields[3])
                stream = __import__('io').BytesIO(raw)
                for _ in range(gv.read_compact(stream)):
                    kind, = struct.unpack('<I', gv.take(stream, 4))
                    h = gv.take(stream, 32)[::-1].hex()
                    if kind in (17, 18) and h not in self.fetched:
                        self.fetched.add(h)
                        wanted.append((kind, h))
        self.scan = len(self.lines)
        if not wanted:
            return
        start = len(self.lines)
        payload = p2a.compact(len(wanted)) + b''.join(
            struct.pack('<I', kind) + bytes.fromhex(h)[::-1] for kind, h in wanted)
        self.send(n, 'getdata', payload)
        self.settle()
        out = self.lines[:start + 1]
        for line in self.lines[start + 1:]:
            out.append(line)
            fields = line.split()
            if len(fields) == 4 and fields[:2] == ['<', str(n)] and fields[2] in ('govobj', 'govobjvote'):
                raw = bytes.fromhex(fields[3])
                own = (parse_govobj(raw)[1] if fields[2] == 'govobj' else raw[:36]) == self.mn_outpoint
                if own:
                    out.append(f'expect-{fields[2]} {n} {fields[3]}')
        self.lines = out
        self.scan = len(self.lines)

    def set_time(self, t):
        self.now = t
        super().set_time(t)

    def settle_relay(self):
        """Settle with a quiet period longer than the 1 s governance relay task
        (net_governance.cpp:69-77), so a tip's announcements usually land in its own segment."""
        previous = p2a.QUIET
        p2a.QUIET = RELAY_QUIET
        try:
            self.settle()
        finally:
            p2a.QUIET = previous

    def tip(self, count=1):
        hashes = []
        for _ in range(count):
            hashes += self.generate(1)
            self.node.rpc('syncwithvalidationinterfacequeue')
            self.settle_relay()
            self.collect()
        return hashes

    def height(self):
        return self.node.rpc('getblockcount')

    def tip_to(self, height):
        if height <= self.height():
            raise RuntimeError(f'already at {self.height()}, asked for {height}')
        return self.tip(height - self.height())

    def payee_tips(self):
        """Recorded `masternode winners`; the tips h at which the recording masternode is the first
        projected payee, i.e. the payee of h + 1."""
        winners = self.rpc_record('masternode', 'winners')['result']
        mine = address(self.setup['operators'][RECORDER]['payout'])
        return sorted(int(h) - 1 for h, payee in winners.items() if payee.startswith(mine))

    def step_clock(self, seconds):
        """Advance in steps under the 20 min peer and ping timeouts (net.cpp:2322-2330), answering the
        node's pings before each step, so no peer drops and mnsync is not reset (net.cpp:2224-2231)."""
        while seconds:
            for n, peer in self.peers.items():
                if peer.last_ping is not None and not peer.closed:
                    self.pong(n)
            step = min(seconds, PEER_STEP)
            self.advance(step)
            seconds -= step

    def deliver_object(self, h, raw):
        self.fetched.add(h)
        self.deliver(17, h, raw.hex() if isinstance(raw, bytes) else raw)
        self.settle()

    def deliver_vote(self, h, raw):
        self.fetched.add(h)
        self.deliver(18, h, raw.hex())
        self.settle()

    def restart(self, at=None):
        """`restart T`: the node stops and starts again on the same datadir and arguments, with
        -mocktime=T (by default the current mock time, so no clock jump crosses the restart)."""
        at = self.now if at is None else at
        for p in self.peers.values():
            p.sock.close()
        self.peers.clear()
        self.node.stop()
        self.flush_logs()
        mark = len(self.lines)
        self.lines.append(f'restart {at}')
        self.peer_ids.clear()
        self.scores.clear()
        self.now = at
        self.node.args = [f'-mocktime={at}' if a.startswith('-mocktime=') else a for a in self.node.args]
        self.node.stderr = open(self.node.dir / 'restart-stderr.txt', 'wb')  # noqa: SIM115
        self.node.proc = subprocess.Popen(self.node.args, stdout=subprocess.DEVNULL, stderr=self.node.stderr)
        deadline = time.monotonic() + 120
        while True:
            try:
                self.node.rpc('getblockcount')
                break
            except (OSError, RuntimeError):
                if self.node.proc.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError('restart failed') from None
                time.sleep(0.2)
        self.flush_logs()
        self.set_time(self.now)
        self.blockchain_synced()
        self.peer()
        self.finish_sync()
        self.scan = min(self.scan, mark)

    def blockchain_synced(self):
        """A masternode accepts no inbound peer before mnsync's blockchain stage (net.cpp:1978-1981)."""
        for _ in range(2):
            if self.node.rpc('mnsync', 'status')['IsBlockchainSynced']:
                return
            self.rpc_record('mnsync', 'next')
        raise RuntimeError('mnsync blockchain stage did not finish')


# ---- cases


def mn_outpoint(setup, label):
    return outpoint(*setup['operators'][label]['collateral'])


def second_trigger(setup, height, payments, time_):
    data = trigger_data(height, payments)
    return govobj(time_, data, 2, mn_outpoint=mn_outpoint(setup, SECOND), signer=SECOND)


def deliver_proposals(t, setup, keys, voters=None):
    for key in keys:
        p = setup['proposals'][key]
        t.deliver_object(p['hash'], p['hex'])
    for key in keys:
        for index in (voters or {}).get(key, [0]):
            label = RECORDER if index == 0 else SECOND
            h, raw = vote(mn_outpoint(setup, label), setup['proposals'][key]['hash'], YES, t.now,
                          ecdsa=f'gs1-voting-{index}')
            t.deliver_vote(h, raw)


def proposal_votes(t, setup, key, indexes):
    for index in indexes:
        label = RECORDER if index == 0 else SECOND
        h, raw = vote(mn_outpoint(setup, label), setup['proposals'][key]['hash'], YES, t.now,
                      ecdsa=f'gs1-voting-{index}')
        t.deliver_vote(h, raw)


def key_change(t, setup):
    """The recording masternode's owner moves its operator key to another label (ProUpRegTx)."""
    info = setup['operators'][RECORDER]
    spare = setup['spare']
    raw = pro_up_reg_tx([(*spare['outpoint'], spare['value'])], info['proTxHash'], REPLACEMENT, info['owner'],
                        info['voting'], info['payout'], spare['label'], P2pkhSigner.from_label(spare['label']))
    t.send(1, 'tx', raw)
    if txid(raw) not in t.rpc_record('getrawmempool')['result']:
        raise RuntimeError('ProUpRegTx not accepted')


def past_median(t):
    """Advance the clock past the tip's median time, so the block mined after an invalidateblock gets a
    new timestamp instead of repeating the invalidated block's median + 1 (a duplicate)."""
    median = t.node.rpc('getblockchaininfo')['mediantime']
    t.advance(max(1, median + 2 - t.now))


def next_slot(t, after, before):
    """The recording masternode's first payee tip in (after, before), from recorded `masternode winners`.
    The projection lists each masternode once (rpc/masternode.cpp:340), so ask after its last payment."""
    found = [h for h in t.payee_tips() if after < h < before]
    if not found:
        raise RuntimeError(f'the recording masternode has no payee tip in ({after}, {before})')
    return found[0]


def window(t, setup):
    """Move the mock clock to the planned window time and connect h1."""
    t.set_time(setup['window_time'])
    return t.tip()


def record(name, data, setup, out):
    if name == 'G6-ibd':
        return record_ibd(data, setup, out)
    s = setup
    h1 = s['tip'] + 1
    sb = s['superblock']
    with observed_node(name, data, setup=s) as node:
        status = node.rpc('masternode', 'status')
        if status['state'] != 'READY':
            raise RuntimeError(f'recording masternode not READY: {status}')
        t = Transcript(name, node, s['time'], s)
        offset = name == 'G9'
        t.blockchain_synced()
        t.peer()
        if offset:
            for n in range(2, 6):
                t.peer(n, outbound=True, offset=600)
            value = t.rpc_record('getnetworkinfo')['result']['timeoffset']
            if value != 600:
                raise RuntimeError(f'expected offset 600, got {value}')
            t.lines.append(f'timeoffset {value}')
        t.finish_sync()
        t.checkpoint('start')
        t.scan = len(t.lines)
        if name in ('G1', 'G7', 'G11', 'G12'):
            deliver_proposals(t, s, ['A'])
            t.checkpoint('approved')
            window(t, s)
            t.checkpoint('first-window-tip')
            if name == 'G1':
                t.tip(sb - h1 - 1)
                t.checkpoint('window-end')
                t.tip()
                t.checkpoint('superblock')
                t.tip(CYCLE - 1)
                t.checkpoint('next-cycle-window')
                t.tip(8)
                t.checkpoint('next-cycle-trigger')
            elif name == 'G7':
                t.tip()
                slot = next_slot(t, h1, sb)
                t.checkpoint('before-restart')
                t.restart()
                t.checkpoint('after-restart')
                t.tip()
                t.checkpoint('first-tip-after-restart')
                t.tip_to(slot)
                t.checkpoint('payee-slot')
            elif name == 'G11':
                hashes = t.tip(4)
                t.checkpoint('before-reorg')
                t.rpc_record('invalidateblock', hashes[1])
                t.checkpoint('reorg-inside-window')
                competing = second_trigger(s, sb, [dict(s['proposals']['A'], amount=COIN // 2)], t.now)
                t.deliver_object(*competing)
                past_median(t)
                t.tip()
                t.checkpoint('reconnected-window')
                t.tip(sb - node.rpc('getblockcount'))
                t.tip()
                t.checkpoint('after-superblock')
                late = second_trigger(s, sb, [dict(s['proposals']['A'], amount=COIN // 4)], t.now)
                t.rpc_record('invalidateblock', node.rpc('getblockhash', sb))
                t.checkpoint('reorg-below-superblock')
                t.deliver_object(*late)
                past_median(t)
                t.tip()
                t.checkpoint('superblock-reconnected')
                t.tip()
                t.checkpoint('after-reconnected-superblock')
            elif name == 'G12':
                t.tip(sb - h1 - 1)
                t.checkpoint('tip-before-superblock')
                late = second_trigger(s, sb, [dict(s['proposals']['A'], amount=COIN // 2)], t.now)
                t.deliver_object(*late)
                t.checkpoint('trigger-between-tips')
                t.tip()
                t.checkpoint('superblock-tip')
        elif name == 'G2':
            deliver_proposals(t, s, ['A'])
            same = second_trigger(s, sb, [s['proposals']['A']], t.now)
            t.deliver_object(*same)
            t.checkpoint('identical-trigger-first')
            window(t, s)
            t.checkpoint('first-window-tip')
            t.tip()
            t.checkpoint('next-tip')
        elif name == 'G3':
            deliver_proposals(t, s, ['A'], voters={'A': []})
            t.set_time(s['window_time'])
            t.tip(sb - h1 + 1)
            t.checkpoint('first-cycle-unfunded')
            proposal_votes(t, s, 'A', [0])
            t.checkpoint('approved')
            t.tip(sb + WINDOW - node.rpc('getblockcount'))
            t.checkpoint('second-window-not-payee')
            same = second_trigger(s, sb + CYCLE, [s['proposals']['A']], t.now)
            t.deliver_object(*same)
            t.checkpoint('identical-trigger')
            t.tip()
            t.checkpoint('voted-yes')
            t.tip(3)
            t.checkpoint('later-tips')
        elif name == 'G4':
            deliver_proposals(t, s, ['A'])
            competing = second_trigger(s, sb, [dict(s['proposals']['A'], amount=COIN // 2)], t.now)
            future = second_trigger(s, sb + CYCLE, [s['proposals']['A']], t.now)
            t.deliver_object(*competing)
            t.deliver_object(*future)
            t.checkpoint('competing-and-future')
            window(t, s)
            t.checkpoint('first-window-tip')
            t.tip(sb - h1)
            t.checkpoint('superblock')
            outdated = second_trigger(s, sb, [dict(s['proposals']['A'], amount=COIN // 4)], t.now)
            t.deliver_object(*outdated)
            t.checkpoint('outdated-trigger')
            t.tip(2)
            t.checkpoint('after-superblock')
        elif name == 'G5':
            deliver_proposals(t, s, ['B', 'C', 'D', 'E1', 'E2'], voters={'B': [0, 1], 'C': [0, 1]})
            t.checkpoint('approved')
            window(t, s)
            t.checkpoint('first-window-tip')
        elif name == 'G6':
            deliver_proposals(t, s, ['A'])
            t.rpc_record('mnsync', 'reset')
            t.checkpoint('unsynced')
            window(t, s)
            t.checkpoint('unsynced-window-tip')
            t.finish_sync()
            t.checkpoint('resynced')
            t.tip()
            t.tip_to(next_slot(t, h1, sb))
            t.checkpoint('in-window-send')
            t.tip_to(sb - 1)
            t.checkpoint('window-end')
            t.tip_to(next_slot(t, sb, sb + CYCLE - WINDOW))
            t.checkpoint('outside-window')
            key_change(t, s)
            t.tip()
            t.checkpoint('not-ready')
            nxt = sb + CYCLE
            live = second_trigger(s, nxt, [dict(s['proposals']['A'], amount=COIN // 2)], t.now)
            t.deliver_object(*live)
            t.checkpoint('not-ready-live-trigger')
            t.tip_to(nxt - WINDOW + 1)
            t.checkpoint('not-ready-window')
        elif name == 'G8':
            deliver_proposals(t, s, ['A'], voters={'A': []})
            same = second_trigger(s, sb, [s['proposals']['A']], t.now)
            t.deliver_object(*same)
            t.checkpoint('trigger-before-approval')
            window(t, s)
            t.checkpoint('voted-no')
            t.step_clock(GOVERNANCE_UPDATE_MIN + 1)
            t.checkpoint('clock-past-update-min')
            if not t.node.rpc('mnsync', 'status')['IsSynced'] or len(t.node.rpc('getpeerinfo')) != 1:
                raise RuntimeError('the clock step dropped the peer or reset mnsync')
            proposal_votes(t, s, 'A', [0])
            t.checkpoint('approved')
            t.tip(2)
            t.checkpoint('no-yes-after-no')
            t.tip(sb + WINDOW + 8 - node.rpc('getblockcount'))
            t.checkpoint('next-cycle')
        elif name == 'G9':
            deliver_proposals(t, s, ['C', 'D'])
            t.checkpoint('approved')
            window(t, s)
            t.checkpoint('first-window-tip')
        elif name == 'G10':
            deliver_proposals(t, s, ['A'])
            competing = second_trigger(s, sb, [dict(s['proposals']['A'], amount=COIN // 2)], t.now)
            t.deliver_object(*competing)
            key_change(t, s)
            t.checkpoint('key-change-pending')
            window(t, s)
            t.checkpoint('key-change-tip')
            t.tip()
            t.checkpoint('after-key-change')
        else:
            raise ValueError(name)
        t.write(out)


def record_ibd(data, setup, out):
    """IBD as the only gate. In IBD the node requests no governance inventory
    (net_processing.cpp:4590-4593), so the approved proposal arrives first, out of IBD. A restart
    with the clock IBD_OFFSET ahead of every block puts the node back in IBD; its startup Init sets
    READY (init.cpp:2539, active/masternode.cpp:112-173), and mnsync is moved to FINISHED, which a
    tip in IBD keeps (sync.cpp:126-127 return before the IBD reset at 134-141). The window tip h1
    (G1's first block, old enough to keep IBD) then reaches the signer only through
    ActiveContext::UpdatedBlockTip, which returns in IBD (context.cpp:95-96)."""
    g1 = (out / 'G1.txt').read_text().splitlines()
    window_block = next(line.split()[1] for line in g1 if line.startswith('submitblock '))
    later = setup['time'] + IBD_OFFSET
    with observed_node('G6-ibd', data, setup=setup) as node:
        status = node.rpc('masternode', 'status')
        if status['state'] != 'READY':
            raise RuntimeError(f'recording masternode not READY: {status}')
        t = Transcript('G6-ibd', node, setup['time'], setup)
        t.blockchain_synced()
        t.peer()
        t.finish_sync()
        t.checkpoint('start')
        t.scan = len(t.lines)
        deliver_proposals(t, setup, ['A'])
        t.checkpoint('approved')
        if setup['tip'] + 1 not in t.payee_tips():
            raise RuntimeError('the recording masternode is not the payee at the window tip')
        t.restart(later)
        t.checkpoint('ibd-ready-synced')
        info = t.rpc_record('getblockchaininfo')['result']
        state = t.rpc_record('masternode', 'status')['result']['state']
        if not info['initialblockdownload'] or state != 'READY':
            raise RuntimeError(f'G6-ibd needs IBD and READY after the restart: {info["initialblockdownload"]} {state}')
        t.block(window_block)
        node.rpc('syncwithvalidationinterfacequeue')
        t.settle_relay()
        t.collect()
        t.checkpoint('ibd-window-tip')
        if not t.rpc_record('getblockchaininfo')['result']['initialblockdownload']:
            raise RuntimeError('G6-ibd left IBD')
        t.write(out)


# ---- selftest


EXTRA_RPC = ('masternode',)


def parse_gs1(text):
    """Validate GS1's records, then hand the rest to the GV1b parser unchanged."""
    lines = text.splitlines()
    passed = []
    operator_seen = None
    last = {}
    expects = []
    for index, line in enumerate(lines, 1):
        fields = line.split(' ')
        try:
            if fields[0] == 'operator':
                if len(fields) != 4 or operator_seen is not None:
                    raise ValueError('operator arity or repeat')
                if re.fullmatch(r'[a-z0-9-]{1,64}', fields[1]) is None:
                    raise ValueError('operator label')
                if re.fullmatch(r'[0-9a-f]{64}', fields[2]) is None:
                    raise ValueError('operator proTxHash')
                match = re.fullmatch(r'([0-9a-f]{64}):(0|[1-9][0-9]*)', fields[3])
                if match is None:
                    raise ValueError('operator outpoint')
                operator_seen = outpoint(match[1], int(match[2]))
                passed.append('# ' + line)
                continue
            if fields[0] == 'restart':
                if len(fields) != 2 or re.fullmatch(r'0|[1-9][0-9]*', fields[1]) is None:
                    raise ValueError('restart needs its startup mock time')
                passed.append('restart')
                continue
            if fields[0] in ('expect-govobj', 'expect-govobjvote'):
                if len(fields) != 3 or re.fullmatch(r'[1-9][0-9]*', fields[1]) is None:
                    raise ValueError('expect arity')
                if re.fullmatch(r'(?:[0-9a-f]{2})+', fields[2]) is None:
                    raise ValueError('expect hex')
                cmd = fields[0][len('expect-'):]
                if last.get((fields[1], cmd)) != (fields[2], len(passed) - 1):
                    raise ValueError('expect does not follow the matching outgoing message')
                raw = bytes.fromhex(fields[2])
                mn = parse_govobj(raw)[1] if cmd == 'govobj' else raw[:36]
                if operator_seen is None or mn != operator_seen:
                    raise ValueError('expect names another masternode')
                expects.append((cmd, fields[2]))
                passed.append('# ' + line)
                continue
            if fields[0] == '<' and len(fields) == 4 and fields[2] in ('govobj', 'govobjvote'):
                last[(fields[1], fields[2])] = (fields[3], len(passed))
                raw = bytes.fromhex(fields[3])
                mn = parse_govobj(raw)[1] if fields[2] == 'govobj' else raw[:36]
                follows = index < len(lines) and lines[index] == f'expect-{fields[2]} {fields[1]} {fields[3]}'
                if operator_seen is not None and mn == operator_seen and not follows:
                    raise ValueError('signer message without its expect record')
            if fields[0] == 'check' and len(fields) > 2 and fields[1] == 'rpc':
                value = json.loads(line.split(' ', 2)[2])
                if value.get('method') in EXTRA_RPC:
                    if set(value) not in ({'method', 'params', 'result'}, {'method', 'params', 'error'}):
                        raise ValueError('RPC check keys')
                    passed.append('# ' + line)
                    continue
        except (ValueError, KeyError, TypeError, struct.error) as e:
            raise ValueError(f'line {index}: {e}: {line[:100]}') from e
        passed.append(line)
    if operator_seen is None:
        raise ValueError('missing operator record')
    if re.search(r'-masternodeblsprivkey=[0-9a-f]', text):
        raise ValueError('operator secret in args')
    return gv.parse_records('\n'.join(passed)), expects


def check_setup(path):
    with gzip.open(path, 'rt') as f:
        setup = json.load(f)
    if set(setup) != {'blocks', 'time', 'window_time', 'tip', 'superblock', 'budget', 'operators',
                      'proposals', 'spare', 'base'}:
        raise ValueError(f'{path.name}: keys')
    if setup['tip'] != setup['base'] + len(setup['blocks']) or setup['superblock'] != setup['tip'] + 1 + WINDOW:
        raise ValueError(f'{path.name}: heights')
    for block in setup['blocks']:
        if re.fullmatch(r'(?:[0-9a-f]{2})+', block) is None or len(block) < 160:
            raise ValueError(f'{path.name}: block hex')
    if sorted(setup['operators']) != [RECORDER, SECOND]:
        raise ValueError(f'{path.name}: operators')
    for label, info in setup['operators'].items():
        if re.fullmatch(r'[0-9a-f]{64}', info['proTxHash']) is None or info['collateral'] != [info['proTxHash'], 0]:
            raise ValueError(f'{path.name}: {label} registration')
    for key, p in setup['proposals'].items():
        h, _ = govobj(p['time'], p['data'].encode(), 1, collateral=bytes.fromhex(p['collateral'])[::-1])
        if h != p['hash']:
            raise ValueError(f'{path.name}: proposal {key} hash')
    print(f'{path.name}: {len(setup["blocks"])} blocks to {setup["tip"]}, superblock {setup["superblock"]}, '
          f'{len(setup["proposals"])} proposals')


def check_provenance(out):
    text = (out / 'PROVENANCE.md').read_text()
    for path in sorted(out.iterdir()):
        if path.name == 'PROVENANCE.md':
            continue
        row = f'| `{path.name}` | {path.stat().st_size} | `{hashlib.sha256(path.read_bytes()).hexdigest()}` |'
        if row not in text:
            raise ValueError(f'PROVENANCE.md: no matching row for {path.name}')
    for needed in ('regtest', 'contrib/node/gs1-transcripts/capture.py', 'dashd v24.0.0 release',
                   f'{FUNDING_SOURCE}:{FUNDING_LINE}', 'PRIV_KEYS[0]', 'Label scheme'):
        if needed not in text:
            raise ValueError(f'PROVENANCE.md: missing {needed}')


def selftest(out):
    known = {f'{c}.txt' for c in CASES} | {SETUP, 'PROVENANCE.md'}
    stray = sorted(p.name for p in out.iterdir() if p.name not in known)
    if stray:
        raise ValueError(f'unexpected files in {out}: {stray}')
    files = sorted(p for p in out.glob('G*.txt'))
    if not files:
        raise RuntimeError('no G fixtures to validate')
    check_setup(out / SETUP)
    check_provenance(out)
    total = 0
    for path in files:
        records, expects = parse_gs1(path.read_text())
        checks = [r for r in records if r[:2] == ('check', 'gobject')]
        scores = [r for r in records if r[:2] == ('check', 'score')]
        if not checks or [r[2]['label'] for r in checks] != [r[2]['label'] for r in scores]:
            raise ValueError(f'{path.name}: missing checkpoint scores')
        total += len(records) + len(expects)
        print(f'{path.name}: {len(records)} records, {sum(c == "govobj" for c, _ in expects)} signer objects, '
              f'{sum(c == "govobjvote" for c, _ in expects)} signer votes')
    op = 'operator gs1-operator-0 ' + '11' * 32 + ' ' + '22' * 32 + ':0\n'
    vote_hex = '22' * 32 + '00000000' + '33' * 32 + '01000000' + '01000000' + '00' * 8 + '00'
    invalid = ['accept 1\n', op + op, 'operator Bad ' + '11' * 32 + ' ' + '22' * 32 + ':0\n',
               op + 'expect-govobjvote 1 ' + vote_hex,
               op + 'accept 1\n< 1 govobjvote ' + vote_hex + '\n',
               op + 'accept 1\n< 1 govobjvote ' + vote_hex + '\nexpect-govobjvote 1 ' + vote_hex[:-2] + '01',
               op.replace(':0', ':x'), op + '# args: -masternodeblsprivkey=' + 'ab' * 32 + '\n',
               op + 'restart\n', op + 'restart 12x\n']
    for text in invalid:
        try:
            parse_gs1(text)
        except ValueError:
            continue
        raise AssertionError(f'malformed control accepted: {text[:80]}')
    parse_gs1(op + 'accept 1\n< 1 govobjvote ' + vote_hex + '\nexpect-govobjvote 1 ' + vote_hex + '\n')
    for path in files:
        if re.search(r'masternodeblsprivkey=[0-9a-f]{8}', path.read_text()):
            raise AssertionError(f'{path.name}: operator secret')
    print(f'--selftest: {len(files)} G fixtures, {total} records; {len(invalid)} negative controls passed')


# ---- check and provenance


PROVENANCE = """# GS1 signer recordings (dashd v24.0.0, regtest)

- **Network:** regtest, under `-mocktime`; ports below 32768.
- **Recorder:** `contrib/node/gs1-transcripts/capture.py`, run with `python3 -I`
  from the venv the GV1b reproduction steps build.
- **Core version:** dashd v24.0.0 release,
  `~/.rust-dashcore-test/dashcore-24.0.0/bin/dashd`, binary SHA256
  `{binary}`.
- **Bootstrap:** the frozen GV1b chain,
  `dashd-governance/tests/fixtures/dashd-v24.0.0/bootstrap.json.gz` (179 blocks),
  then `G.setup.json.gz`, the blocks `--build-setup` added. Every case replays
  both into a fresh wallet-disabled release node before its transcript starts.
- **Record grammar:** GV1b's, as
  `dashd-governance/tests/fixtures/dashd-v24.0.0/PROVENANCE.md` defines it, plus
  four records:
  - `operator LABEL PROTX TXID:N`: the recording masternode's label, its
    ProRegTx hash and its collateral outpoint;
  - `expect-govobj N HEX` and `expect-govobjvote N HEX`: the message on the line
    before it, sent to peer N, is the recording masternode's own object or vote.
    Replays must emit these bytes;
  - `restart T` replaces GV1b's bare `restart`: the node stops and starts again
    on the same datadir and arguments, except `-mocktime=T`. G7 restarts at the
    current mock time, so no clock jump crosses the restart; G6-ibd restarts
    three days ahead, into IBD. The `time T` record that follows repeats T.

## Keys

- **Label scheme (Q4).** Each key is derived from a fixed label: the scalar is
  `sha256(label)` as a big-endian integer, reduced modulo the curve order (the
  BLS12-381 group order for operator keys, secp256k1's for the others).
  - `gs1-operator-0`: the recording masternode's operator key (ProTx service
    `127.0.0.1:{mn_port}`). Its node runs with this key.
  - `gs1-operator-1`: the second masternode (service `127.0.0.1:{second_port}`,
    no node). It signs every trigger the recording masternode did not build:
    - G2: the trigger identical to M's plan, delivered first;
    - G3: the identical trigger for the second cycle (Core does not check that
      its signer is the projected payee, and operator-1 is not shown to be);
    - G4: the competing, the not-yet-mature (next cycle) and the outdated trigger;
    - G6: the live competing trigger for the next cycle, delivered while M is
      not READY;
    - G8: the identical trigger M votes NO on before the proposal is approved;
    - G10: the competing trigger;
    - G11: the competing trigger after the first reorg and the late one after
      the second;
    - G12: the trigger delivered between tips h-1 and h.
  - `gs1-operator-2`: the operator key G6 and G10 move the recording
    masternode to (ProUpRegTx).
  - `gs1-owner-N`, `gs1-voting-N`, `gs1-payout-N`, `gs1-collateral-N`,
    `gs1-change-N`: owner, voting, payout, collateral and change keys of
    masternode N; `gs1-change-proposals` and `gs1-payee-*` for the proposals.
- **Funding.** The two 1,000 DASH collaterals, the proposal collaterals and their
  fees spend the bootstrap's coinbase outputs (blocks 1-179) to
  `{funding_address}`. Its key is Core's public test-framework value
  `PRIV_KEYS[0]`, `{funding_source}:{funding_line}` at Core v24.0.0. The recorder
  reads it from that file at run time (`--core`, a Dash Core git checkout with
  the `v24.0.0` tag), checks that it derives the address above and holds it in
  memory only. These fixtures carry its public key and signatures (in the
  setup blocks) and nothing else derived from it. Every block the setup and the
  cases mine pays its coinbase to `OP_TRUE`. The G6/G10 ProUpRegTx fee spends
  the recording masternode's registration change output (`gs1-change-0`).

## Replay contract

- **Relay batching.** dashd announces governance objects and votes from a
  relay task that runs every 1 s of wall time on regtest
  (`governance/net_governance.cpp:69-77`). The recorder waits {relay_quiet} s of
  quiet after each tip, but under load an item can still move into another
  `inv` or past the next record. A replay compares the announced inventory,
  the fetched `getdata` items and the signer's `govobj`/`govobjvote` messages as
  a multiset per checkpoint segment (the records between two `check gobject`
  lines), not `inv` message boundaries or positions inside a segment. `--check`
  compares the same way. Inside a segment that spans several tips, the multiset
  does not pin the tip a signer message came at; the decision lines, assigned
  by the log-alignment rule below, pin it.
- **Log alignment.** `log` records are collected when a checkpoint runs and at
  shutdown or restart, not interleaved with messages. Within them the signer
  for tip h (`ActiveContext`, registered first, `init.cpp:2167,2194`) logs
  before `CGovernanceManager::UpdatedBlockTip -- nCachedBlockHeight: h`. Assign
  decision lines by position, not by height value: the lines after one
  `nCachedBlockHeight:` line, up to and including the next, belong to the tip
  that next line reports. Startup after a restart can log the same height twice
  (G7, 211), the heights go backwards at reorgs (G11), and G6-ibd logs no such
  line (no decision line is expected there).
- **Decision lines.** The signer lines a replay compares, per tip:
  `CreateSuperblockCandidate`, `CreateGovernanceTrigger --`,
  `VoteGovernanceTriggers --` and `VoteFundingTrigger --`.

## What each case pins

Every claim not listed below shows in the signer's own messages (`expect-*`)
or in the checkpoint JSON. These claims show only in the decision lines,
because a signer missing the named check would send the same bytes:

| Case | Claim | Decision lines the replay must match |
|---|---|---|
| G6 | outside the window (tips 220-225, M not the payee) | `CreateGovernanceTrigger --`: none from 220 to 229 (`signing.cpp:42-44` returns first); a signer without the gate logs `we are not the payee, skipping` at 221-225. Tip 226, M's payee tip outside the window, shows in messages: no `expect-govobj` |
| G6 | not READY (227-231, with operator-1's live trigger for 240) | `VoteGovernanceTriggers --` / `VoteFundingTrigger --`: none (`signing.cpp:184` returns silently); a signer without the gate logs `Voting NO-FUNDING for trigger:<live> failed` at 227-231, and its vote, for a removed masternode, is rejected (`object.cpp:461-467`). `CreateGovernanceTrigger -- we are not the payee, skipping` at 230 and 231 is Core's: the plan and payee check do not test READY |
| G7 | the restart restores voted-yes | `VoteGovernanceTriggers -- Not voting YES-FUNDING for trigger:<M's>, we voted yes for it already` at the first tip after the restart (212). From 212 through M's payee tip 218, each tip logs `VoteGovernanceTriggers -- Not voting NO-FUNDING for trigger:<M's>, we voted yes for it already` (`signing.cpp:251-256`) and no `CreateSuperblockCandidate`, `CreateGovernanceTrigger --` or YES-FUNDING line (`signing.cpp:45` returns first). A signer without the record check re-sends the stored YES, which dashd drops (same vote; `object.cpp:443-456`) |
| G8 | no YES after M's NO (the decision; the missing vote also shows in messages after the clock step) | `VoteGovernanceTriggers -- Not voting YES-FUNDING for trigger:<h>, we voted no for it already` at each window tip |
| G10 | no votes at the key-change tip | no `VoteGovernanceTriggers --` / `VoteFundingTrigger --` line at h; a stale-key NO on the competing trigger is rejected (`object.cpp:461-467`) and logs `failed`. `CreateGovernanceTrigger -- we are not the payee, skipping` at h is Core's (null proTxHash) |
| G11 | no re-creation after the reorg to 211 (voted-yes kept) | no `CreateGovernanceTrigger --` or `Not voting YES-FUNDING` line at the reconnected tips 212-219 (`signing.cpp:45` returns first); a signer that lost voted-yes logs `Not voting YES-FUNDING for trigger:<M's>, we voted yes for it already`. After the second reorg, below the superblock (voted-yes was reset at 220), that line at 219 is Core's restore path |

- **G6 unsynced** (tip 210) shows in the checkpoint: relay is silent while
  unsynced (`governance.cpp:112-140`), but a trigger built anyway would be in
  `unsynced-window-tip`'s object list.
- **G6-ibd** isolates IBD. In IBD dashd requests no governance inventory
  (`net_processing.cpp:4590-4593`), so the proposal and M's vote arrive out of
  IBD; a `restart` three days ahead puts the node in IBD, its startup Init
  sets READY (`init.cpp:2539`, `active/masternode.cpp:112-173`), and `mnsync
  next` reaches FINISHED, which a tip in IBD keeps (`masternode/sync.cpp:126-127`
  return before the IBD reset at 134-141). At the window tip, where M is the
  first payee, only `active/context.cpp:95-96` stops the signer: a signer
  without it would add a trigger, which the `ibd-window-tip` checkpoint would
  list.
- **G5** pins both orderings. The equal-yes E pair (`--build-setup` suffixes
  the names until this holds) sorts `{g5_arith}` first by `UintToArith256`
  (Core's order, `governance.cpp:1252-1256`; it fills the budget and the other
  does not fit) and `{g5_memcmp}` first by memcmp, so a memcmp sort pays the
  other one. C `{g5_c}` has two yes votes and is filled first, but its hash is
  below the arith winner's, so the final sort (`signing.cpp:113-115`) puts the
  winner before C in the trigger.

## Reproduction

```sh
DASHVERSION=24.0.0 python3 -I contrib/setup-dashd.py
# venv: as in dashd-governance/tests/fixtures/dashd-v24.0.0/PROVENANCE.md
VENV/bin/python3 -I contrib/node/gs1-transcripts/capture.py --build-setup --core ~/workspace/dash
VENV/bin/python3 -I contrib/node/gs1-transcripts/capture.py
VENV/bin/python3 -I contrib/node/gs1-transcripts/capture.py --check
VENV/bin/python3 -I contrib/node/gs1-transcripts/capture.py --selftest
```

`--build-setup` is the only step that reads the funding key. A rebuilt setup
changes every hash and must be followed by every case. `--check` re-records
G1 from the frozen setup and compares it with the stored file, excluding
runtime ports, the version nonce and MNAUTH challenge, ping nonces, log wall
timestamps and `verificationprogress`, and comparing relayed items per
checkpoint segment (see Replay contract).

## Files

| File | Bytes | SHA256 |
|---|---|---|
{rows}
"""


def normalize(text):
    """GV1b's masks, plus the sim peer's ephemeral source port in dashd's log lines only."""
    return ''.join(re.sub(r'(peer 127\.0\.0\.1:)\d+', r'\1PORT', line) if line.startswith('log ') else line
                   for line in gv.normalize(text).splitlines(True))


BATCHED = ('inv', 'getdata', 'govobj', 'govobjvote')


def by_segment(text):
    """Relay batching follows the 1 s wall-clock relay task (net_governance.cpp:69-77), so --check
    compares, per checkpoint segment, the multiset of announced and fetched items and of signer
    messages, not inv/getdata message boundaries or their position within the segment."""
    out, pool = [], []
    for line in text.splitlines():
        fields = line.split(' ')
        if fields[0] in ('expect-govobj', 'expect-govobjvote'):
            pool.append(line)
            continue
        if len(fields) == 4 and fields[0] in ('<', '>') and fields[2] in BATCHED and (fields[0], fields[2]) != ('>', 'govobj') \
                and (fields[0], fields[2]) != ('>', 'govobjvote'):
            if fields[2] in ('inv', 'getdata'):
                stream = __import__('io').BytesIO(bytes.fromhex(fields[3]))
                for _ in range(gv.read_compact(stream)):
                    pool.append(f'{fields[0]} {fields[1]} {fields[2]}-item {gv.take(stream, 36).hex()}')
            else:
                pool.append(line)
            continue
        if line.startswith('check gobject '):
            out.extend(sorted(pool))
            pool.clear()
        out.append(line)
    out.extend(sorted(pool))
    return '\n'.join(out) + '\n'


def provenance(out):
    files = sorted(p for p in out.iterdir() if p.name.startswith('G') and p.is_file())
    rows = '\n'.join(f'| `{p.name}` | {p.stat().st_size} | `{hashlib.sha256(p.read_bytes()).hexdigest()}` |'
                     for p in files)
    with gzip.open(out / SETUP, 'rt') as f:
        proposals = json.load(f)['proposals']
    pair = (proposals['E1']['hash'], proposals['E2']['hash'])
    (out / 'PROVENANCE.md').write_text(PROVENANCE.format(
        g5_arith=max(pair, key=lambda h: int(h, 16)), g5_memcmp=max(pair, key=memcmp_key), g5_c=proposals['C']['hash'],
        relay_quiet=RELAY_QUIET,
        binary=hashlib.sha256(DASHD.read_bytes()).hexdigest(), rows=rows, mn_port=MN_PORT,
        second_port=SECOND_PORT, funding_address=FUNDING_ADDRESS, funding_source=FUNDING_SOURCE,
        funding_line=FUNDING_LINE))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, default=FIXTURES)
    parser.add_argument('--build-setup', action='store_true')
    parser.add_argument('--core', type=Path, default=Path.home() / 'workspace/dash',
                        help='Dash Core git checkout with the v24.0.0 tag (--build-setup only)')
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--selftest', action='store_true')
    parser.add_argument('--no-provenance', action='store_true')
    parser.add_argument('--provenance', action='store_true', help='only rewrite PROVENANCE.md')
    parser.add_argument('cases', nargs='*', choices=CASES)
    args = parser.parse_args()
    os.umask(0o077)
    if args.selftest:
        selftest(args.out)
        return
    if args.provenance:
        provenance(args.out)
        return
    ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    with gzip.open(GV1B / 'bootstrap.json.gz', 'rt') as f:
        data = json.load(f)
    args.out.mkdir(parents=True, exist_ok=True)
    if args.build_setup:
        build_setup(data, args.out, funding_key(args.core))
    with gzip.open(args.out / SETUP, 'rt') as f:
        setup = json.load(f)
    if args.check:
        print('Excluded from comparison: runtime ports; version nonce and MNAUTH challenge; '
              'ping nonce; log wall timestamp; verificationprogress (wall-clock estimate). '
              'inv/getdata batches and signer messages compared as a multiset per checkpoint segment.')
        with tempfile.TemporaryDirectory(prefix='check-', dir=ROOT) as directory:
            record('G1', data, setup, Path(directory))
            before = by_segment(normalize((args.out / 'G1.txt').read_text()))
            after = by_segment(normalize((Path(directory) / 'G1.txt').read_text()))
            if before != after:
                sys.stdout.writelines(difflib.unified_diff(before.splitlines(True), after.splitlines(True),
                                                          fromfile='fixture', tofile='recapture'))
                raise SystemExit('--check: G1 differs')
        print('--check: G1 matches')
        return
    for name in args.cases or ([] if args.build_setup else CASES):
        print(f'Capturing {name}', flush=True)
        started = time.monotonic()
        record(name, data, setup, args.out)
        print(f'{name}: {round(time.monotonic() - started, 1)} s', flush=True)
    if not args.no_provenance:
        provenance(args.out)


if __name__ == '__main__':
    main()
