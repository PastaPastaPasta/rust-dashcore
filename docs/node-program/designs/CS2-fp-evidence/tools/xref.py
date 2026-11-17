#!/usr/bin/env python3
"""Cross-reference tool for dashd release binaries: ELF (x86_64, aarch64, riscv64),
PE32+ (win64) and Mach-O (x86_64, arm64).

Function bounds come from .eh_frame FDEs (ELF), the .pdata exception table (PE) or
LC_FUNCTION_STARTS (Mach-O). Data references are resolved from RIP-relative operands
(x86_64), adrp/add/ldr pairs (arm64) and auipc/addi/load pairs (riscv64). Calls into
shared libraries are named through the PLT (ELF), the IAT (PE) or __stubs (Mach-O).

Usage:
  xref.py BIN index                      # build the xref index (cached as BIN.xref.json)
  xref.py BIN str TEXT                   # functions referencing a NUL-terminated string containing TEXT
  xref.py BIN dbl VALUE                  # functions referencing an 8-byte double VALUE
  xref.py BIN func ADDR                  # annotated disassembly of the function containing ADDR
  xref.py BIN calls NAME                 # functions calling the imported function NAME
  xref.py BIN sym NAME                   # Mach-O only: address of the first symbol containing NAME
"""
import json
import os
import struct
import sys

import capstone
from capstone import arm64 as A64
from capstone import riscv as RV
from capstone import x86 as X86

CALL_MNEMONICS = ("call", "bl", "jmp", "b", "jal", "c.jal", "j", "c.j", "tail")


class Bin:
    def __init__(self, path):
        self.path = path
        with open(path, "rb") as fh:
            self.raw = fh.read()
        self.symbols = {}  # addr -> name (Mach-O only)
        self.iat = {}  # PE: IAT slot address -> imported name
        self.plt = {}  # stub address -> imported name (ELF PLT, Mach-O __stubs)
        if self.raw[:4] == b"\x7fELF":
            self._init_elf()
        elif self.raw[:2] == b"MZ":
            self._init_pe()
        elif self.raw[:4] == b"\xcf\xfa\xed\xfe":
            self._init_macho()
        else:
            raise SystemExit(f"{path}: unknown format")
        if self.arch == "x86_64":
            self.cs = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
        elif self.arch == "arm64":
            self.cs = capstone.Cs(capstone.CS_ARCH_ARM64, capstone.CS_MODE_ARM)
        else:
            self.cs = capstone.Cs(capstone.CS_ARCH_RISCV, capstone.CS_MODE_RISCV64 | capstone.CS_MODE_RISCVC)
        self.cs.detail = True
        if self.fmt == "elf":
            self.plt = self._elf_plt()

    # ---- format back ends --------------------------------------------------

    def _init_elf(self):
        from elftools.elf.elffile import ELFFile

        self.fmt = "elf"
        self.f = open(self.path, "rb")
        self.elf = ELFFile(self.f)
        self.arch = {"EM_X86_64": "x86_64", "EM_AARCH64": "arm64", "EM_RISCV": "riscv64"}[self.elf["e_machine"]]
        self.regions = []
        # Section labels include SHT_NOBITS sections (.tbss overlaps .data/.got), as the
        # first version of this tool did, so earlier listings regenerate byte-identically.
        self.labels = []
        for s in self.elf.iter_sections():
            if s["sh_addr"]:
                self.labels.append((s["sh_addr"], s["sh_addr"] + s["sh_size"], s.name))
            if s["sh_addr"] and s["sh_type"] != "SHT_NOBITS":
                self.regions.append((s["sh_addr"], s["sh_addr"] + s["sh_size"], s.name, s["sh_offset"]))
        text = self.elf.get_section_by_name(".text")
        self.text_lo, self.text_hi = text["sh_addr"], text["sh_addr"] + text["sh_size"]
        self.const_sections = (".rodata",)

    def _init_pe(self):
        self.fmt = "pe"
        self.arch = "x86_64"
        pe = struct.unpack_from("<I", self.raw, 0x3C)[0]
        nsec, optsz = struct.unpack_from("<HH", self.raw, pe + 6)[0], struct.unpack_from("<H", self.raw, pe + 20)[0]
        opt = pe + 24
        self.imagebase = struct.unpack_from("<Q", self.raw, opt + 24)[0]
        ddir = opt + 112  # PE32+ data directories
        self.regions = []
        sh = opt + optsz
        for i in range(nsec):
            name = self.raw[sh:sh + 8].rstrip(b"\0").decode()
            vsz, va, rsz, roff = struct.unpack_from("<IIII", self.raw, sh + 8)
            self.regions.append((self.imagebase + va, self.imagebase + va + min(vsz, rsz), name, roff))
            if name == ".text":
                self.text_lo, self.text_hi = self.imagebase + va, self.imagebase + va + vsz
            sh += 40
        self.const_sections = (".rdata",)
        # Exception directory (index 3): RUNTIME_FUNCTION {begin, end, unwind} RVAs.
        exc_rva, exc_size = struct.unpack_from("<II", self.raw, ddir + 3 * 8)
        self._pe_funcs = []
        for k in range(exc_size // 12):
            b, e, _u = struct.unpack("<III", self.read(self.imagebase + exc_rva + 12 * k, 12))
            if b and e > b:
                self._pe_funcs.append((self.imagebase + b, self.imagebase + e))
        self._pe_funcs.sort()
        # Import directory (index 1): name every IAT slot.
        imp_rva, _imp_size = struct.unpack_from("<II", self.raw, ddir + 1 * 8)
        d = self.imagebase + imp_rva
        while True:
            ilt, _ts, _fc, name_rva, iat = struct.unpack("<IIIII", self.read(d, 20))
            if not name_rva:
                break
            lookup = ilt or iat
            k = 0
            while True:
                (ent,) = struct.unpack("<Q", self.read(self.imagebase + lookup + 8 * k, 8))
                if not ent:
                    break
                if not ent >> 63:
                    nm = self.cstring(self.imagebase + (ent & 0x7FFFFFFF) + 2)
                    self.iat[self.imagebase + iat + 8 * k] = nm
                k += 1
            d += 20

    def _init_macho(self):
        import lief

        self.fmt = "macho"
        mo = lief.MachO.parse(self.path).at(0)
        self.arch = {"CPU_TYPE.ARM64": "arm64", "CPU_TYPE.X86_64": "x86_64"}[str(mo.header.cpu_type)]
        self.regions = []
        for sec in mo.sections:
            if sec.size and sec.offset:
                self.regions.append((sec.virtual_address, sec.virtual_address + sec.size, sec.name, sec.offset))
            if sec.name == "__text":
                self.text_lo, self.text_hi = sec.virtual_address, sec.virtual_address + sec.size
        self.const_sections = ("__const", "__literal8", "__literal16", "__literals")
        base = mo.imagebase
        starts = sorted({base + off for off in mo.function_starts.functions} | {self.text_hi})
        self._macho_funcs = [(a, b) for a, b in zip(starts, starts[1:]) if self.text_lo <= a < self.text_hi]
        for s in mo.symbols:
            if s.value and s.name and self.text_lo <= s.value < self.text_hi:
                self.symbols.setdefault(s.value, s.name)
        ind = list(mo.dynamic_symbol_command.indirect_symbols)
        self.plt = {}
        for sec in mo.sections:
            if sec.name in ("__stubs", "__auth_stubs") and sec.reserved2:
                for k in range(sec.size // sec.reserved2):
                    sym = ind[sec.reserved1 + k]
                    self.plt[sec.virtual_address + k * sec.reserved2] = sym.name.lstrip("_")

    # ---- memory ------------------------------------------------------------

    def read(self, addr, n):
        for lo, hi, _name, off in self.regions:
            if lo <= addr < hi:
                return self.raw[off + addr - lo: off + addr - lo + min(n, hi - addr)]
        return None

    def section_of(self, addr):
        for lo, hi, name in getattr(self, "labels", None) or [(r[0], r[1], r[2]) for r in self.regions]:
            if lo <= addr < hi:
                return name
        return None

    def cstring(self, addr, limit=200):
        b = self.read(addr, limit)
        if b is None:
            return None
        end = b.find(b"\0")
        if end < 0:
            return None
        try:
            s = b[:end].decode()
        except UnicodeDecodeError:
            return None
        return s if s and all(32 <= ord(c) < 127 or c in "\n\t" for c in s) else None

    # ---- ELF PLT -------------------------------------------------------------

    def _elf_plt(self):
        """Map PLT stub address -> imported symbol name."""
        out = {}
        dynsym = self.elf.get_section_by_name(".dynsym")
        relplt = self.elf.get_section_by_name(".rela.plt")
        if relplt is None:
            return out
        got_to_name = {}
        for r in relplt.iter_relocations():
            got_to_name[r["r_offset"]] = dynsym.get_symbol(r["r_info_sym"]).name
        for name in (".plt", ".plt.sec"):
            plt = self.elf.get_section_by_name(name)
            if plt is None:
                continue
            code = plt.data()
            base = plt["sh_addr"]
            insns = list(self.cs.disasm(code, base))
            if self.arch == "x86_64":
                for ins in insns:
                    if ins.mnemonic in ("jmp", "bnd jmp") and ins.op_str.startswith("qword ptr [rip"):
                        tgt = ins.address + ins.size + ins.operands[0].mem.disp
                        if tgt in got_to_name:
                            out[ins.address & ~0xF] = got_to_name[tgt]
            elif self.arch == "arm64":
                for i, ins in enumerate(insns):
                    if ins.mnemonic == "adrp" and i + 1 < len(insns):
                        page = ins.operands[1].imm
                        nxt = insns[i + 1]
                        if nxt.mnemonic == "ldr" and nxt.operands[1].type == A64.ARM64_OP_MEM:
                            tgt = page + nxt.operands[1].mem.disp
                            if tgt in got_to_name:
                                out[ins.address] = got_to_name[tgt]
            else:  # riscv64: auipc t3, hi; ld t3, lo(t3); jalr t1, t3; nop
                for i, ins in enumerate(insns):
                    if ins.mnemonic == "auipc" and i + 1 < len(insns):
                        hi = _sext20(ins.operands[1].imm) << 12
                        nxt = insns[i + 1]
                        if nxt.operands and nxt.operands[-1].type == RV.RISCV_OP_MEM:
                            tgt = ins.address + hi + nxt.operands[-1].mem.disp
                            if tgt in got_to_name:
                                out[ins.address] = got_to_name[tgt]
        return out

    # ---- functions -------------------------------------------------------------

    def functions(self):
        if self.fmt == "elf":
            dw = self.elf.get_dwarf_info()
            out = []
            for e in dw.EH_CFI_entries():
                if type(e).__name__ == "FDE":
                    lo = e.header["initial_location"]
                    out.append((lo, lo + e.header["address_range"]))
            out.sort()
            return [(lo, hi) for lo, hi in out if self.text_lo <= lo < self.text_hi]
        if self.fmt == "pe":
            return self._pe_funcs
        return self._macho_funcs

    def disasm(self, lo, hi):
        code = self.read(lo, hi - lo)
        return list(self.cs.disasm(code, lo))

    # ---- references --------------------------------------------------------------

    def scan(self, insns):
        """Yield ('data', ins, addr) and ('call', ins, target-or-name) events."""
        if self.arch == "x86_64":
            for ins in insns:
                for op in ins.operands:
                    if op.type == X86.X86_OP_MEM and op.mem.base == X86.X86_REG_RIP:
                        a = ins.address + ins.size + op.mem.disp
                        if ins.mnemonic in ("call", "jmp") and a in self.iat:
                            yield "call", ins, self.iat[a]
                        else:
                            yield "data", ins, a
                if ins.mnemonic in ("call", "jmp") and ins.operands and ins.operands[0].type == X86.X86_OP_IMM:
                    yield "call", ins, ins.operands[0].imm
            return
        if self.arch == "arm64":
            pages = {}
            for ins in insns:
                ops = ins.operands
                if ins.mnemonic in ("bl", "b") and ops and ops[0].type == A64.ARM64_OP_IMM:
                    yield "call", ins, ops[0].imm
                if ins.mnemonic == "adrp":
                    pages[ops[0].reg] = ops[1].imm
                    continue
                if ins.mnemonic == "adr":
                    yield "data", ins, ops[1].imm
                    continue
                if ins.mnemonic == "add" and len(ops) == 3 and ops[1].type == A64.ARM64_OP_REG and ops[1].reg in pages and ops[2].type == A64.ARM64_OP_IMM:
                    yield "data", ins, pages[ops[1].reg] + ops[2].imm
                    continue
                for op in ops:
                    if op.type == A64.ARM64_OP_MEM and op.mem.base in pages:
                        yield "data", ins, pages[op.mem.base] + op.mem.disp
                # a write to a tracked register invalidates it (conservative: first operand)
                if ops and ops[0].type == A64.ARM64_OP_REG and ops[0].reg in pages and ins.mnemonic not in ("str", "stp", "strb", "strh", "cmp", "tst", "cbz", "cbnz"):
                    del pages[ops[0].reg]
            return
        # riscv64
        hi = {}
        for ins in insns:
            ops = ins.operands
            m = ins.mnemonic
            if m == "auipc":
                hi[ops[0].reg] = ins.address + (_sext20(ops[1].imm) << 12)
                continue
            if m in ("jal", "c.jal", "j", "c.j") and ops and ops[-1].type == RV.RISCV_OP_IMM:
                yield "call", ins, ins.address + ops[-1].imm
                continue
            if m in ("jalr", "c.jalr") and ops:
                last = ops[-1]
                if last.type == RV.RISCV_OP_MEM and last.mem.base in hi:
                    yield "call", ins, hi[last.mem.base] + last.mem.disp
                elif len(ops) >= 2 and ops[-1].type == RV.RISCV_OP_IMM and ops[-2].type == RV.RISCV_OP_REG and ops[-2].reg in hi:
                    yield "call", ins, hi[ops[-2].reg] + ops[-1].imm
            elif m in ("addi", "c.addi") and len(ops) == 3 and ops[1].type == RV.RISCV_OP_REG and ops[1].reg in hi and ops[2].type == RV.RISCV_OP_IMM:
                yield "data", ins, hi[ops[1].reg] + ops[2].imm
            else:
                for op in ops:
                    if op.type == RV.RISCV_OP_MEM and op.mem.base in hi:
                        yield "data", ins, hi[op.mem.base] + op.mem.disp
            # a write to a tracked register (first operand of anything but a store/branch) invalidates it
            if ops and ops[0].type == RV.RISCV_OP_REG and ops[0].reg in hi and not m.startswith(("s", "c.s", "fs", "c.fs", "b", "c.b")):
                del hi[ops[0].reg]

    def import_name(self, target):
        """Name of the imported function a call target reaches, or None."""
        if isinstance(target, str):
            return target
        if target in self.plt:
            return self.plt[target]
        if self.fmt == "pe" and self.text_lo <= target < self.text_hi:
            # mingw import thunk: jmp qword ptr [rip + iat]
            insn = next(self.cs.disasm(self.read(target, 16) or b"", target), None)
            if insn and insn.mnemonic == "jmp" and insn.operands and insn.operands[0].type == X86.X86_OP_MEM and insn.operands[0].mem.base == X86.X86_REG_RIP:
                return self.iat.get(insn.address + insn.size + insn.operands[0].mem.disp)
        return None


def _sext20(v):
    v &= 0xFFFFF
    return v - 0x100000 if v & 0x80000 else v


def build_index(b):
    idx = []
    for lo, hi in b.functions():
        insns = b.disasm(lo, hi)
        data, calls = set(), set()
        for kind, _ins, v in b.scan(insns):
            if kind == "data":
                data.add(v)
            else:
                nm = b.import_name(v)
                if nm:
                    calls.add(nm)
        idx.append({"lo": lo, "hi": hi, "data": sorted(data), "plt": sorted(calls)})
    return idx


def load_index(b):
    cache = b.path + ".xref.json"
    if os.path.exists(cache):
        with open(cache) as fh:
            return json.load(fh)
    idx = build_index(b)
    with open(cache, "w") as fh:
        json.dump(idx, fh)
    return idx


def find_const(b, pattern):
    hits = []
    for lo, hi, name, off in b.regions:
        if name not in b.const_sections + (".data.rel.ro", ".data", "__data", "__cstring"):
            continue
        data = b.raw[off: off + hi - lo]
        start = 0
        while True:
            i = data.find(pattern, start)
            if i < 0:
                break
            hits.append(lo + i)
            start = i + 1
    return hits


def fn_containing(idx, addr):
    for e in idx:
        if e["lo"] <= addr < e["hi"]:
            return e
    return None


def annotate(b, lo, hi, idx=None):
    insns = b.disasm(lo, hi)
    notes, targets = {}, {}
    for kind, ins, v in b.scan(insns):
        if kind == "call":
            targets[ins.address] = v
            continue
        a = v
        sec = b.section_of(a)
        note = f"-> {a:#x} [{sec}]"
        s = b.cstring(a)
        if s:
            note += f" str {s!r}"
        raw = b.read(a, 16)
        if raw and len(raw) >= 8 and sec in b.const_sections:
            d = struct.unpack("<d", raw[:8])[0]
            note += f" f64 {d!r} (0x{struct.unpack('<Q', raw[:8])[0]:016x})"
            if len(raw) == 16:
                note += f" hi64 0x{struct.unpack('<Q', raw[8:])[0]:016x}"
        notes[ins.address] = note
    out = []
    for ins in insns:
        line = f"{ins.address:8x}:  {ins.mnemonic:8s} {ins.op_str}"
        t = targets.get(ins.address)
        if t is not None:
            nm = b.import_name(t)
            if nm:
                line += f"    <{nm}@plt>"
            elif isinstance(t, int) and idx is not None and not (lo <= t < hi):
                e = fn_containing(idx, t)
                if e:
                    sym = b.symbols.get(e["lo"])
                    line += f"    <fn {e['lo']:#x}{' ' + sym if sym else ''}>"
        if ins.address in notes:
            line += "    ; " + notes[ins.address]
        out.append(line)
    return "\n".join(out)


def main():
    b = Bin(sys.argv[1])
    cmd = sys.argv[2]
    if cmd == "index":
        idx = load_index(b)
        print(len(idx), "functions")
        return
    if cmd == "sym":
        hits = sorted(a for a, n in b.symbols.items() if sys.argv[3] in n)
        for a in hits[:1]:
            print(f"{a:#x} {b.symbols[a]}")
        return
    idx = load_index(b)
    if cmd == "str":
        needle = sys.argv[3].encode()
        targets = set()
        for lo, hi, name, off in b.regions:
            if name not in (".rodata", ".rdata", "__cstring", "__const"):
                continue
            data = b.raw[off: off + hi - lo]
            start = 0
            while True:
                i = data.find(needle, start)
                if i < 0:
                    break
                j = i
                while j > 0 and data[j - 1] != 0:
                    j -= 1
                targets.add(lo + j)
                start = i + 1
        for t in sorted(targets):
            users = [e for e in idx if t in e["data"]]
            print(f"{t:#x} {b.cstring(t)!r}: " + ", ".join(f"fn {e['lo']:#x}-{e['hi']:#x}" for e in users))
    elif cmd == "dbl":
        pat = struct.pack("<d", float(sys.argv[3]))
        for t in find_const(b, pat):
            users = [e for e in idx if t in e["data"]]
            print(f"{t:#x}: " + ", ".join(f"fn {e['lo']:#x}-{e['hi']:#x}" for e in users))
    elif cmd == "func":
        a = int(sys.argv[3], 16)
        e = fn_containing(idx, a)
        sym = b.symbols.get(e["lo"])
        print(f"; function {e['lo']:#x}-{e['hi']:#x} plt={e['plt']}" + (f" sym={sym}" if sym else ""))
        print(annotate(b, e["lo"], e["hi"], idx))
    elif cmd == "calls":
        name = sys.argv[3]
        for e in idx:
            if name in e["plt"]:
                print(f"fn {e['lo']:#x}-{e['hi']:#x} ({e['hi']-e['lo']} bytes) plt={e['plt']}")


if __name__ == "__main__":
    main()
