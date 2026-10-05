"""Mach-O report tables and bounded ARM64 tracing for clpc.py."""

import bisect
import hashlib
import re
import struct
import subprocess
import uuid
from collections import deque
from dataclasses import dataclass
from pathlib import Path

from capstone import CS_ARCH_ARM64, CS_MODE_ARM, Cs
from capstone.arm64 import ARM64_OP_FP, ARM64_OP_IMM, ARM64_OP_MEM, ARM64_OP_REG


class Image:
    def __init__(self, path):
        self.data = Path(path).read_bytes()
        if self.data[:4] != b"\xcf\xfa\xed\xfe":
            raise ValueError("Expected a decompressed 64-bit Mach-O")
        self.segments = self.read_segments(0)
        self.base = min(s[1] for s in self.segments)
        self.entries = []
        for kind, off in self.commands(0):
            if kind == 0x80000035:
                head = self.unpack("<Q", off + 16)[0]
                name = self.string(off + self.unpack("<I", off + 24)[0])
                self.entries.append((name, head))
        self.symbols = []
        # nm supplies function boundaries; import stubs need their indirect table.
        for line in subprocess.check_output(
            ["nm", "-n", str(path)], text=True
        ).splitlines():
            match = re.match(r"([0-9a-f]{16}) ([TtSsbDd]) (.+)", line)
            if match:
                self.symbols.append((int(match[1], 16), match[3], match[2]))
        for head in [0, *(head for _, head in self.entries)]:
            symtab = indirect = None
            stubs = []
            for kind, off in self.commands(head):
                if kind == 2:
                    symtab = self.unpack("<IIII", off + 8)
                if kind == 0xB:
                    indirect = self.unpack("<I", off + 56)[0]
                if kind == 0x19:
                    for index in range(self.unpack("<I", off + 64)[0]):
                        section = off + 72 + index * 80
                        if self.string(section) in ("__stubs", "__auth_stubs"):
                            stubs.append(
                                (
                                    *self.unpack("<QQ", section + 32),
                                    *self.unpack("<II", section + 68),
                                )
                            )
            if symtab and indirect is not None:
                symoff, _, stroff, _ = symtab
                for addr, length, first, stride in stubs:
                    for index in range(length // stride):
                        symbol = self.unpack("<I", indirect + (first + index) * 4)[0]
                        if symbol & 0xC0000000:
                            continue
                        name = self.string(
                            stroff + self.unpack("<I", symoff + symbol * 16)[0]
                        )
                        self.symbols.append(
                            (addr + index * stride, name + " [stub]", "T")
                        )
        self.symbols.sort()
        self.addresses = [s[0] for s in self.symbols]

    def unpack(self, fmt, off):
        return struct.unpack_from(fmt, self.data, off)

    def string(self, off):
        return self.data[off : self.data.index(b"\0", off)].decode()

    def commands(self, head):
        off = head + 32
        for _ in range(self.unpack("<I", head + 16)[0]):
            kind, size = self.unpack("<II", off)
            if size < 8 or off + size > len(self.data):
                raise ValueError("Invalid Mach-O load command")
            yield kind, off
            off += size

    def read_segments(self, head):
        return [
            (name.rstrip(b"\0").decode(), addr, vmsize, fileoff, size)
            for kind, off in self.commands(head)
            if kind == 0x19
            for name, addr, vmsize, fileoff, size in [self.unpack("<16sQQQQ", off + 8)]
        ]

    def offset_for(self, addr):
        for _, vm, _, off, size in self.segments:
            if vm <= addr < vm + size:
                return off + addr - vm
        return None

    def location(self, addr):
        index = bisect.bisect_right(self.addresses, addr) - 1
        if index < 0:
            return hex(addr)
        base, name, _ = self.symbols[index]
        return f"{name}+{addr - base:#x}"

    def drivers(self):
        for bundle, head in self.entries:
            if not re.fullmatch(r"com.apple.driver.AppleT.*CLPC(?:v[0-9]+)?", bundle):
                continue
            local = []
            uid = None
            for kind, off in self.commands(head):
                if kind == 0x1B:
                    uid = str(uuid.UUID(bytes=self.data[off + 8 : off + 24]))
                if kind == 2:
                    sym, count, strings, _ = self.unpack("<IIII", off + 8)
                    for index in range(count):
                        name, typ, _, _, addr = self.unpack("<IBBHQ", sym + index * 16)
                        if typ & 0xEE == 0xE and addr:
                            local.append((addr, self.string(strings + name)))
            addresses = sorted({addr for addr, _ in local})

            def named(suffix):
                return one(list({a for a, n in local if n.endswith(suffix)}), suffix)

            def length(addr):
                return addresses[bisect.bisect_right(addresses, addr)] - addr

            names, actions, reports = (
                named(s)
                for s in (
                    "119merged_report_namesE",
                    "121merged_report_actionsE",
                    "4CLPC7reportsE",
                )
            )
            count = length(actions) // 24
            require(count > 0 and length(actions) % 24 == 0, "Invalid report actions")
            stride = length(names) // count
            require(
                stride in (72, 80) and length(names) % count == 0,
                "Unknown report-name layout",
            )
            channels = []
            for index in range(count):
                name = self.offset_for(names + index * stride)
                action = self.offset_for(actions + index * 24)
                targets = [
                    self.base + (v & 0x3FFFFFFF) for v in self.unpack("<QQQ", action)
                ]
                ident = (index << 32) | self.unpack("<I", name + 64)[0]
                channels.append(
                    {
                        "id": f"0x{ident:016x}",
                        "name": self.string(name),
                        "storage": hex(targets[0]),
                        "reports_offset": hex(targets[0] - reports),
                        "configure": self.location(targets[1]),
                        "generate": self.location(targets[2]),
                    }
                )
            yield {
                "bundle": bundle,
                "uuid": uid,
                "count": count,
                "stride": stride,
                "channels": channels,
                "segments": {
                    name: {
                        "address": hex(addr),
                        "virtual_size": vm,
                        "size": size,
                        "sha256": hashlib.sha256(
                            self.data[off : off + size]
                        ).hexdigest(),
                    }
                    for name, addr, vm, off, size in self.read_segments(head)
                    if name != "__LINKEDIT"
                },
            }

    def energy_nodes(self):
        decoder = Cs(CS_ARCH_ARM64, CS_MODE_ARM)
        decoder.detail = decoder.skipdata = True
        rows = []
        for start, name, kind in self.symbols:
            if kind not in "Tt" or not re.search(
                r"clpc5power13EnergySampler(4initEv|20pmgrPopulateNodeInfo)", name
            ):
                continue
            end = self.addresses[bisect.bisect_right(self.addresses, start)]
            off = self.offset_for(start)
            regs, candidates = {}, set()
            for ins in decoder.disasm(self.data[off : off + end - start], start):
                if not ins.id:
                    regs.clear()
                    continue
                ops = ins.operands
                if ins.mnemonic == "adrp":
                    regs[ops[0].reg] = ops[1].imm & ((1 << 64) - 1)
                elif (
                    ins.mnemonic == "add"
                    and len(ops) == 3
                    and ops[1].reg in regs
                    and ops[2].type == ARM64_OP_IMM
                ):
                    addr = regs[ops[1].reg] + ops[2].imm
                    regs[ops[0].reg] = addr
                    candidates.add(addr)
                elif "initEv" in name and ins.mnemonic in ("ldr", "ldur"):
                    operand = ops[-1]
                    if (
                        operand.type == ARM64_OP_MEM
                        and operand.mem.base in regs
                        and not operand.mem.index
                    ):
                        candidates.add(regs[operand.mem.base] + operand.mem.disp)
            for addr in sorted(candidates):
                off = self.offset_for(addr)
                if off is None:
                    continue
                if "initEv" in name:
                    values = []
                    for i in range(32):
                        x, y, z = self.unpack("<III", off + i * 12)
                        if not 4 <= x < 64 or y > 3 or z > 3:
                            break
                        values.append((x, y, z))
                    if len(values) >= 4:
                        rows.append((start, "PMGR_ENUMS", addr, values))
                else:
                    values = {}
                    for i in range(32):
                        ptr = self.base + (
                            self.unpack("<Q", off + i * 8)[0] & 0x3FFFFFFF
                        )
                        pos = self.offset_for(ptr)
                        value = (
                            self.data[pos : pos + 180].split(b"\0")[0]
                            if pos is not None
                            else b""
                        )
                        values[str(i)] = (
                            value.decode()
                            if len(value) > 2 and all(32 <= c < 127 for c in value)
                            else ""
                        )
                    if "ANE" in values.values() and "GPU" in values.values():
                        rows.append((start, "NODE_NAMES", addr, values))
        return rows


@dataclass(frozen=True)
class Index:
    source: int | None = None
    scale: int = 1
    offset: int = 0


@dataclass(frozen=True)
class Ptr:
    base: str
    offset: int
    indexed: bool | Index = False


@dataclass(frozen=True)
class Poison:
    pointer: Ptr


EMPTY = frozenset()
BAD = frozenset({"?"})


def labels(v):
    if isinstance(v, frozenset):
        return v
    if isinstance(v, tuple):
        return frozenset().union(*(labels(x) for x in v))
    return EMPTY


def join(a, b):
    if a == b:
        return a
    # Clang's checked pointer arithmetic selects a poisoned address on overflow.
    # That arm faults; only the matching unpoisoned address can supply a value.
    if isinstance(a, Poison) and a.pointer == b:
        return b
    if isinstance(b, Poison) and b.pointer == a:
        return a
    if isinstance(a, tuple) or isinstance(b, tuple):
        a = a if isinstance(a, tuple) else (a,) * 4
        b = b if isinstance(b, tuple) else (b,) * 4
        return tuple(join(x, y) for x, y in zip(a, b))
    if labels(a) or labels(b):
        return labels(a) | labels(b)
    return None


def plus(a, b):
    if isinstance(b, Index):
        if isinstance(a, int):
            a = Ptr("image", a)
        if isinstance(a, Ptr) and not a.indexed:
            return Ptr(a.base, a.offset + b.offset, Index(b.source, b.scale))
    if isinstance(a, Ptr):
        if isinstance(b, int):
            return Ptr(a.base, a.offset + b, a.indexed)
        return Ptr(a.base, a.offset, True)
    if isinstance(a, int) and isinstance(b, int):
        return (a + b) & ((1 << 64) - 1)
    if labels(a) or labels(b):
        return labels(a) | labels(b)
    return None


def shift(value, amount):
    if isinstance(value, int):
        return value << amount
    if value is None:
        value = Index()
    if isinstance(value, Index):
        return Index(value.source, value.scale << amount, value.offset << amount)
    return BAD if labels(value) else None


class Flow:
    def __init__(
        self,
        image,
        start,
        end,
        nodes,
        acc_count,
        helper=None,
        *,
        inputs=(),
        indices=None,
        reports=(),
    ):
        self.image = image
        md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)
        md.detail = True
        md.skipdata = True
        off = self.image.offset_for(start)
        if off is None or end <= start:
            raise ValueError("Function is outside mapped executable storage")
        self.ins = {
            i.address: i
            for i in md.disasm(self.image.data[off : off + end - start], start)
        }
        self.acc_count = acc_count
        self.start = start
        self.end = end
        self.nodes = nodes
        self.helper = helper
        self.calls = set()
        self.stores = {}
        self.unknown = set()
        self.accesses = {}
        self.inputs = inputs
        self.indices = indices or {}
        self.reports = set(reports)
        self.report_writes = {}
        self.indexed_stores = set()
        self.arguments = set()
        self.loops = []
        for pc, ins in self.ins.items():
            prev = self.ins.get(pc - 4)
            if ins.mnemonic == "b.ne" and prev and prev.mnemonic == "cmp":
                ops = prev.operands
                target = ins.operands[0].imm & ((1 << 64) - 1)
                if (
                    target < pc
                    and len(ops) == 2
                    and ops[1].type == ARM64_OP_IMM
                    and 0 < ops[1].imm <= 128
                ):
                    self.loops.append((target, pc, self.key(prev, ops[0].reg)))

    def key(self, ins, reg):
        name = ins.reg_name(reg)
        if name == "fp":
            name = "x29"
        if name == "lr":
            name = "x30"
        if re.fullmatch("[wxsqdv][0-9]+", name):
            return ("v" if name[0] in "sqdv" else "x") + name[1:]
        return name

    def get(self, ins, op, rf):
        if op.type == ARM64_OP_IMM:
            return (op.imm << op.shift.value) & ((1 << 64) - 1)
        if op.type == ARM64_OP_FP:
            return 0
        if op.type != ARM64_OP_REG:
            return None
        name = ins.reg_name(op.reg)
        k = self.key(ins, op.reg)
        if name in ("xzr", "wzr"):
            return 0
        v = rf.get(k)
        if k.startswith("v"):
            vs = v if isinstance(v, tuple) else (v,) * 4
            if op.vector_index >= 0:
                return vs[op.vector_index]
            if name.startswith("s"):
                return vs[0]
            if name.startswith("d"):
                return join(vs[0], vs[1])
            return vs
        if isinstance(v, int) and name.startswith("w"):
            v &= 0xFFFFFFFF
        if op.shift.value:
            v = shift(v, op.shift.value)
        return v

    def put(self, ins, op, rf, v):
        k = self.key(ins, op.reg)
        name = ins.reg_name(op.reg)
        if name in ("xzr", "wzr"):
            return
        if k.startswith("v"):
            if op.vector_index >= 0:
                old = rf.get(k)
                vs = list(old) if isinstance(old, tuple) else [old] * 4
                vs[op.vector_index] = v
                v = tuple(vs)
            elif name.startswith("s"):
                v = (v, 0, 0, 0)
            elif name.startswith("d"):
                v = (v, v, 0, 0)
            elif not isinstance(v, tuple):
                v = (v,) * 4
        rf[k] = v

    def address(self, ins, op, rf):
        b = rf.get(self.key(ins, op.mem.base))
        a = plus(b, op.mem.disp)
        if op.mem.index:
            ix = rf.get(self.key(ins, op.mem.index))
            ix = shift(ix, op.shift.value)
            a = plus(a, ix)
        return a

    def width(self, ins, op):
        name = ins.reg_name(op.reg)
        return {"q": 16, "v": 16, "d": 8, "x": 8, "s": 4, "w": 4}.get(name[:1], 8)

    def readword(self, a, mem):
        if isinstance(a, Ptr):
            if a.base == "image" and isinstance(a.indexed, Index):
                if a.offset in self.indices:
                    return Index(a.offset)
                return BAD
            if isinstance(a.indexed, Index) and a.indexed.source in self.indices:
                start = self.indices[a.indexed.source]
                if (
                    a.base == "sample"
                    and a.indexed.scale == 16
                    and start <= a.offset < start + 16
                ):
                    return frozenset({"CPU"}) if a.offset - start < 8 else EMPTY
            if a in mem and not a.indexed:
                return mem[a]
            for start, words in self.inputs:
                if a.base == "sample" and start <= a.offset < start + 8 * len(words):
                    if a.indexed and len(set(words)) != 1:
                        return BAD
                    return frozenset({words[(a.offset - start) // 8]})
            if a.base == "raw":
                if (
                    8 * len(self.nodes)
                    <= a.offset
                    < 8 * (len(self.nodes) + self.acc_count)
                ):
                    return frozenset({"CPU"})
                if not 0 <= a.offset < 8 * len(self.nodes):
                    return BAD
                if a.indexed:
                    return BAD
                return frozenset({self.nodes[a.offset // 8]})
            if a.indexed:
                return BAD
            return mem.get(a)
        if isinstance(a, int):
            report = next((r for r in self.reports if r <= a < r + 8), None)
            if report is not None:
                return frozenset({f"counter:{report:x}"})
            off = self.image.offset_for(a)
            return (
                struct.unpack_from("<I", self.image.data, off)[0]
                if off is not None
                else 0
            )
        return BAD

    def load(self, a, width, mem):
        vals = tuple(self.readword(plus(a, i), mem) for i in range(0, width, 4))
        if width == 16:
            return vals
        if width == 8:
            if isinstance(vals[0], Ptr):
                return vals[0]
            if all(isinstance(v, int) for v in vals):
                return vals[0] | (vals[1] << 32)
            return join(*vals)
        return vals[0]

    def store(self, a, width, v, mem, pc):
        if isinstance(a, int) and a in self.reports and width == 8:
            self.report_writes[(pc, a)] = join(self.report_writes.get((pc, a)), v)
        if isinstance(a, Ptr) and a.indexed:
            self.indexed_stores.add((pc, a, width))
        if not isinstance(a, Ptr) or a.indexed:
            return
        if isinstance(v, tuple):
            vs = v
        elif isinstance(v, int):
            vs = tuple((v >> (i * 32)) & 0xFFFFFFFF for i in range(width // 4))
        else:
            vs = (v,) * (width // 4)
        for i, value in enumerate(vs):
            addr = plus(a, i * 4)
            mem[addr] = value
            if a.base in ("out", "sample"):
                key = (pc, addr.offset)
                self.stores[key] = join(self.stores.get(key), value)

    def run(self, root="raw", registers=None):
        initial = (
            {
                "sp": Ptr("stack", 0),
                "x0": Ptr(root, 0) if isinstance(root, str) else root,
                "x8": Ptr("out", 0),
                "x2": Ptr("sample", 0),
                **(registers or {}),
            },
            {},
        )

        def state_key(pc, rf):
            return (
                pc,
                tuple(rf.get(reg) for lo, hi, reg in self.loops if lo <= pc <= hi),
            )

        first = state_key(self.start, initial[0])
        states = {first: initial}
        pending = deque([first])
        steps = 0
        while pending:
            state = pending.popleft()
            pc = state[0]
            steps += 1
            if steps > 100000:
                raise ValueError("dataflow did not converge")
            ins = self.ins.get(pc)
            if ins is None:
                continue
            rf, mem = states[state]
            rf = rf.copy()
            mem = mem.copy()
            m = ins.mnemonic
            o = ins.operands if ins.id else []
            succ = [pc + 4]
            if m in ("ret", "retab", "retaa"):
                succ = []
            elif m == "b":
                succ = [o[0].imm & ((1 << 64) - 1)]
            elif m.startswith("b.") or m in ("cbz", "cbnz", "tbz", "tbnz"):
                target = o[-1].imm & ((1 << 64) - 1)
                zero = rf.get("zero")
                if m in ("b.eq", "b.ne") and isinstance(zero, bool):
                    succ = [target if zero == (m == "b.eq") else pc + 4]
                else:
                    succ.append(target)
            elif m in ("adrp", "adr"):
                self.put(ins, o[0], rf, self.get(ins, o[1], rf))
            elif m in ("mov", "fmov"):
                self.put(ins, o[0], rf, self.get(ins, o[1], rf))
            elif m == "movz":
                self.put(ins, o[0], rf, self.get(ins, o[1], rf))
            elif m == "movk":
                old = self.get(ins, o[0], rf)
                v = self.get(ins, o[1], rf)
                amount = o[1].shift.value
                self.put(
                    ins,
                    o[0],
                    rf,
                    ((old & ~(0xFFFF << amount)) | v)
                    if isinstance(old, int)
                    else Poison(old)
                    if isinstance(old, Ptr) and amount == 48 and v == 0x2BAD << 48
                    else None,
                )
            elif m == "movi":
                self.put(ins, o[0], rf, 0)
            elif m in ("add", "sub", "adds", "subs"):
                a = self.get(ins, o[1], rf)
                b = self.get(ins, o[2], rf)
                if m.startswith("sub"):
                    b = -b if isinstance(b, int) else None
                value = plus(a, b)
                self.put(ins, o[0], rf, value)
                if m.endswith("s"):
                    rf["zero"] = value == 0 if isinstance(value, int) else None
            elif m == "lsl":
                amount = self.get(ins, o[2], rf)
                self.put(
                    ins,
                    o[0],
                    rf,
                    shift(self.get(ins, o[1], rf), amount)
                    if isinstance(amount, int) and amount < 64
                    else None,
                )
            elif m in ("orr", "eor", "and", "ands"):
                a, b = (self.get(ins, op, rf) for op in o[1:])
                if isinstance(a, int) and isinstance(b, Index):
                    a, b = b, a
                value = None
                if isinstance(a, int) and isinstance(b, int):
                    value = a | b if m == "orr" else a ^ b if m == "eor" else a & b
                if (
                    m == "orr"
                    and isinstance(a, Index)
                    and isinstance(b, int)
                    and 0 <= b < a.scale
                    and not a.offset
                ):
                    value = Index(a.source, a.scale, b)
                self.put(ins, o[0], rf, value)
                if m == "ands":
                    rf["zero"] = value == 0 if isinstance(value, int) else None
            elif m in ("ldr", "ldur", "ldrb", "ldrh", "ldrsw", "ldp"):
                mo = o[-1] if o[-1].type == ARM64_OP_MEM else o[-2]
                a = self.address(ins, mo, rf)
                self.accesses.setdefault(pc, set()).add(a)
                if ins.writeback and mo.mem.disp:
                    rf[self.key(ins, mo.mem.base)] = a
                count = 2 if m == "ldp" else 1
                w = self.width(ins, o[0])
                for n in range(count):
                    self.put(ins, o[n], rf, self.load(plus(a, n * w), w, mem))
                if ins.writeback and o[-1].type == ARM64_OP_IMM:
                    rf[self.key(ins, mo.mem.base)] = plus(a, o[-1].imm)
            elif m in ("str", "stur", "strb", "strh", "stp"):
                mo = o[-1] if o[-1].type == ARM64_OP_MEM else o[-2]
                a = self.address(ins, mo, rf)
                self.accesses.setdefault(pc, set()).add(a)
                if ins.writeback and mo.mem.disp:
                    rf[self.key(ins, mo.mem.base)] = a
                count = 2 if m == "stp" else 1
                w = self.width(ins, o[0])
                for n in range(count):
                    self.store(plus(a, n * w), w, self.get(ins, o[n], rf), mem, pc)
                if ins.writeback and o[-1].type == ARM64_OP_IMM:
                    rf[self.key(ins, mo.mem.base)] = plus(a, o[-1].imm)
            elif m in (
                "fmul",
                "fdiv",
                "fadd",
                "fsub",
                "fmadd",
                "fmsub",
                "fmax",
                "fmin",
                "fmaxnm",
                "fminnm",
                "fcvt",
                "fcvtzu",
                "ucvtf",
                "scvtf",
                "fneg",
                "fabs",
            ):
                vs = [self.get(ins, x, rf) for x in o[1:]]
                if any(isinstance(v, tuple) for v in vs):
                    vs = [v if isinstance(v, tuple) else (v,) * 4 for v in vs]
                    value = tuple(
                        frozenset().union(*(labels(v[n]) for v in vs)) for n in range(4)
                    )
                else:
                    value = frozenset().union(*(labels(v) for v in vs))
                self.put(ins, o[0], rf, value)
            elif m in ("fcsel", "csel"):
                self.put(
                    ins,
                    o[0],
                    rf,
                    join(self.get(ins, o[1], rf), self.get(ins, o[2], rf)),
                )
            elif m in ("bl", "blr", "blraa", "blrab"):
                target = self.get(ins, o[0], rf) if m == "bl" else None
                self.arguments.add(
                    (pc, target, tuple(rf.get(f"x{i}") for i in range(4)))
                )
                rounded = (
                    rf.get("v0")
                    if target is not None
                    and self.image.location(target)
                    in ("_roundf [stub]+0x0", "_roundf+0x0")
                    else None
                )
                if self.helper and target in self.helper:
                    out = self.helper[target]
                    dest = rf.get("x8")
                    for offset, v in out.items():
                        self.store(plus(dest, offset), 4, v, mem, pc)
                    self.calls.add((pc, target, dest, rf.get("x0")))
                # No returning scalar is needed from the helper's diagnostic calls.
                for k in list(rf):
                    if k.startswith("x") and k[1:].isdigit() and int(k[1:]) < 19:
                        rf[k] = None
                    if (
                        k.startswith("v")
                        and k[1:].isdigit()
                        and int(k[1:]) not in range(8, 16)
                    ):
                        rf[k] = None
                rf["zero"] = None
                if rounded is not None:
                    rf["v0"] = tuple(
                        labels(v) | {"rounded"}
                        for v in (
                            rounded if isinstance(rounded, tuple) else (rounded,) * 4
                        )
                    )
            elif m in ("cmp", "tst"):
                a, b = (self.get(ins, op, rf) for op in o[:2])
                rf["zero"] = (
                    (a == b if m == "cmp" else a & b == 0)
                    if isinstance(a, int) and isinstance(b, int)
                    else None
                )
            elif m in (
                "bti",
                "pacibsp",
                "paciasp",
                "autibsp",
                "autiasp",
                "nop",
                "cmn",
                "fcmp",
                "ccmp",
                "fccmp",
            ):
                if m in ("cmn", "fcmp", "ccmp", "fccmp"):
                    rf["zero"] = None
            elif not ins.id:
                word = int.from_bytes(ins.bytes, "little")
                if word & 0xFFFFFC00 == 0xDAC01800:
                    rf["x" + str(word & 31)] = None
                elif word & 0xFFFFFC00 == 0x91C00000:
                    rf["x" + str(word & 31)] = None  # smax Xd, Xn, #0 (time delta)
                elif word == 0xDAC1A7FE:
                    pass  # pacibsppc
                elif word in (0x5520601F, 0x5521305F):
                    succ = []  # retabsppc
                elif word & 0xFFE0E000 == 0x9A002000:
                    a = rf.get("x" + str((word >> 5) & 31))
                    b = rf.get("x" + str((word >> 16) & 31))
                    if isinstance(b, int):
                        b <<= (word >> 10) & 7
                    else:
                        b = shift(b, (word >> 10) & 7)
                    rf["x" + str(word & 31)] = plus(a, b)
                elif word & 0xFFE0E000 == 0x9B602000:
                    a = rf.get("x" + str((word >> 5) & 31))
                    b = rf.get("x" + str((word >> 16) & 31))
                    base = rf.get("x" + str((word >> 10) & 31))
                    product = (
                        a * b if isinstance(a, int) and isinstance(b, int) else None
                    )
                    rf["x" + str(word & 31)] = plus(base, product)
                else:
                    self.unknown.add((pc, hex(word)))
                    succ = []
            else:
                reads, writes = ins.regs_access()
                tainted = any(labels(rf.get(self.key(ins, reg))) for reg in reads)
                for reg in writes:
                    key = self.key(ins, reg)
                    rf[key] = BAD if tainted else None
                    if key == "nzcv":
                        rf["zero"] = None
            for nextpc in succ:
                if nextpc not in self.ins:
                    continue
                nextstate = state_key(nextpc, rf)
                if nextstate not in states:
                    states[nextstate] = (rf.copy(), mem.copy())
                    pending.append(nextstate)
                    continue
                a, b = states[nextstate]
                nr = {k: join(a.get(k), rf.get(k)) for k in a.keys() | rf.keys()}
                nm = {k: join(b.get(k), mem.get(k)) for k in b.keys() | mem.keys()}
                if nr != a or nm != b:
                    states[nextstate] = (nr, nm)
                    pending.append(nextstate)
        outputs = {}
        for (pc, offset), v in self.stores.items():
            outputs[offset] = join(outputs.get(offset), v)
        return outputs, steps


COMPONENTS = {
    frozenset({"CPU"}): "CPU",
    frozenset({"ANE"}): "ANE",
    frozenset({"GPU"}): "GPU",
    frozenset({"GPU", "GPUSRAM"}): "GPU",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def one(values, description):
    require(len(values) == 1, f"Expected one {description}; found {len(values)}")
    return values[0]


def describe(block):
    return [f"{i.address:#x} {i.mnemonic} {i.op_str}" for i in block]


def discover(image, driver, node_rows):
    code = driver["segments"]["__TEXT_EXEC"]
    lo = int(code["address"], 16)
    hi = lo + code["virtual_size"]
    local = [(a, n) for a, n, k in image.symbols if lo <= a < hi and k in "Tt"]
    row = {
        "bundle": driver["bundle"],
        "uuid": driver["uuid"],
        "channels": {},
        "candidates": [],
    }

    def function(fragment):
        return one(
            [(a, n) for a, n in local if fragment in n and ".cold." not in n], fragment
        )

    def flow(addr, **kwargs):
        return Flow(
            image,
            addr,
            image.addresses[bisect.bisect_right(image.addresses, addr)],
            nodes,
            acc_count,
            **kwargs,
        )

    try:
        pmgr_addr, pmgr_name = function("EnergySampler10samplePMGRERNS_5arrayIyLm")
        pmgr_count = int(re.search(r"arrayIyLm(\d+)EEE", pmgr_name)[1])
        enums = one(
            [
                r
                for r in node_rows
                if lo <= r[0] < hi and r[1] == "PMGR_ENUMS" and len(r[3]) == pmgr_count
            ],
            "PMGR descriptor array",
        )
        names = one(
            [r for r in node_rows if lo <= r[0] < hi and r[1] == "NODE_NAMES"],
            "energy-node name array",
        )
        nodes = [names[3][str(r[0])] for r in enums[3]]
        require(all(nodes), "An energy descriptor has no readable name")
        acc_addr, acc_name = function("EnergySampler9sampleACCERNS_5arrayIyLm")
        acc_count = int(re.search(r"arrayIyLm(\d+)EEE", acc_name)[1])
        row["inputs"] = {
            "descriptor_function": hex(enums[0]),
            "descriptor_table": hex(enums[2]),
            "name_function": hex(names[0]),
            "name_table": hex(names[2]),
            "pmgr_nodes": [
                {"descriptor": list(d), "name": n} for d, n in zip(enums[3], nodes)
            ],
            "acc_function": hex(acc_addr),
            "acc_word_count": acc_count,
        }
        helpers = [
            (a, n)
            for a, n in local
            if n.startswith("__ZZN4clpc5power14PackageLimiter") and "updateMetrics" in n
        ]
        helper_outputs = {}
        inputs, indices = (), {}
        if helpers:
            helper_addr, helper_name = one(helpers, "component helper")
            helper = flow(helper_addr)
            helper_out, _ = helper.run()
            require(
                not helper.unknown,
                f"Undecoded helper instructions: {sorted(helper.unknown)}",
            )
            helper_outputs[helper_addr] = helper_out
            row["trace"] = {
                "helper": {"address": hex(helper_addr), "symbol": helper_name},
                "helper_fields": {
                    hex(k): sorted(labels(v))
                    for k, v in sorted(helper_out.items())
                    if labels(v)
                },
            }
        else:
            row["trace"] = {}

        direct = [
            (a, n)
            for a, n in local
            if "DirectAccessEnergySampler" in n
            and "9sampleACC" in n
            and "EnergyNodeSampleELm0E" not in n
        ]
        if not helpers or direct:
            # Derive the sample layout and direct sampler object from call arguments.
            instance = one(
                [
                    a
                    for a, n, _ in image.symbols
                    if n == "__ZN4clpc5_clpcE"
                    and any(
                        int(seg["address"], 16)
                        <= a
                        < int(seg["address"], 16) + seg["virtual_size"]
                        for seg in driver["segments"].values()
                    )
                ],
                "CLPC instance",
            )
            sampling_addr, _ = function("4CLPC22samplePowerAndThermals")
            sampling = flow(sampling_addr)
            sampling.run(instance)

            def arguments(target):
                return {args for _, call, args in sampling.arguments if call == target}

            pmgr_start = one(
                list({args[1] for args in arguments(pmgr_addr)}), "PMGR sample buffer"
            )
            acc_start = one(
                list({args[1] for args in arguments(acc_addr)}), "ACC sample buffer"
            )
            require(
                isinstance(pmgr_start, Ptr)
                and isinstance(acc_start, Ptr)
                and pmgr_start.base == acc_start.base == "stack"
                and not pmgr_start.indexed
                and not acc_start.indexed,
                "Unresolved energy sample buffers",
            )
            sample_start = pmgr_start.offset - 0x10
            inputs = (
                (0x10, nodes),
                (acc_start.offset - sample_start, ["CPU"] * acc_count),
            )
            row["inputs"]["sample_ranges"] = [
                {"offset": hex(start), "words": words} for start, words in inputs
            ]
            for direct_addr, direct_name in direct:
                for args in arguments(direct_addr):
                    table, dest = args[0], args[2]
                    require(
                        isinstance(table, int)
                        and isinstance(dest, Ptr)
                        and dest.base == "stack"
                        and not dest.indexed,
                        "Unresolved direct ACC arguments",
                    )
                    sample_offset = dest.offset - sample_start
                    # Each CPU cluster has two index words. Verify that sampleACC
                    # uses these same words to write energy/time records to its output.
                    candidates = {table: sample_offset, table + 4: sample_offset}
                    proof = flow(direct_addr, indices=candidates)
                    proof.run(
                        table,
                        {"x2": Ptr("acc-output", 0), "x3": Ptr("acc-previous", 0)},
                    )
                    require(
                        not proof.unknown,
                        f"Undecoded direct ACC instructions: {sorted(proof.unknown)}",
                    )
                    stores = {
                        (a.indexed.source, a.offset)
                        for _, a, width in proof.indexed_stores
                        if a.base == "acc-output"
                        and isinstance(a.indexed, Index)
                        and a.indexed.scale == 16
                        and width == 8
                    }
                    require(
                        stores
                        == {
                            (source, offset)
                            for source in candidates
                            for offset in (0, 8)
                        },
                        "Direct ACC does not write the expected energy/time records",
                    )
                    require(
                        all(indices.get(k, v) == v for k, v in candidates.items()),
                        "Direct ACC calls disagree about the sample buffer",
                    )
                    indices.update(candidates)
                    row["trace"].setdefault("direct_acc", []).append(
                        {
                            "function": hex(direct_addr),
                            "symbol": direct_name,
                            "index_words": [hex(a) for a in candidates],
                            "sample_offset": hex(sample_offset),
                            "writers": [
                                hex(pc)
                                for pc, a, _ in sorted(
                                    proof.indexed_stores, key=lambda x: x[0]
                                )
                                if a.base == "acc-output"
                            ],
                        }
                    )

        addr, name = one(
            [
                (a, n)
                for a, n in local
                if n.startswith("__ZN4clpc5power14PackageLimiter")
                and "updateMetrics" in n
            ],
            "updateMetrics",
        )
        reports = {}
        for channel in driver["channels"]:
            if (
                "configureSimpleReportIyE" in channel["configure"]
                and "generateSimpleReportIyE" in channel["generate"]
            ):
                reports.setdefault(int(channel["storage"], 16), []).append(channel)
        caller = flow(
            addr, helper=helper_outputs, inputs=inputs, indices=indices, reports=reports
        )
        outputs, _ = caller.run("package")
        require(
            not caller.unknown,
            f"Undecoded updateMetrics instructions: {sorted(caller.unknown)}",
        )
        if helpers:
            require(
                caller.calls and all(c[3] == Ptr("sample", 0x10) for c in caller.calls),
                "Helper input differs from the recognized ABI",
            )
        row["trace"].update(
            {
                "caller": {"address": hex(addr), "symbol": name},
                "sample_fields": {
                    hex(k): sorted(labels(v))
                    for k, v in sorted(outputs.items())
                    if labels(v)
                },
            }
        )
        assignments = {}
        for (pc, target), value in sorted(caller.report_writes.items()):
            if len(reports[target]) != 1:
                continue
            dependencies = labels(value)
            markers = {f"counter:{target:x}", "rounded"}
            if not markers <= dependencies:
                continue
            dependencies -= markers
            candidate = {
                **reports[target][0],
                "writer": hex(pc),
                "dependencies": sorted(dependencies),
                "evidence": describe(
                    [i for a, i in caller.ins.items() if pc - 32 <= a <= pc]
                ),
            }
            row["candidates"].append(candidate)
            component = COMPONENTS.get(dependencies)
            if component:
                assignments.setdefault(component, []).append(
                    {**candidate, "identification": "automated dependency trace"}
                )
        row["channels"] = {
            component: hits[0]
            for component, hits in assignments.items()
            if len(hits) == 1
        }
        require(
            set(row["channels"]) == {"CPU", "GPU", "ANE"},
            "Incomplete or ambiguous component trace; inspect candidates and sample_fields",
        )
        row["status"] = "identified"
    except (ValueError, KeyError, IndexError, struct.error) as error:
        row["status"] = "needs_review"
        row["error"] = str(error)
    return row
