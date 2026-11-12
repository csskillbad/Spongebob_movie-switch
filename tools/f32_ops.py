#!/usr/bin/env python3
"""Turns single-precision multiply-adds on known-single operands into one f32 fused multiply-add
in crates/game/src/gen.

The recompiler emits fmadds / fmsubs / fnmsubs / fnmadds / ps_madd* as an unfused double
expression rounded to single, e.g. `(f1 * f2 + f3) as f32 as f64`. LLVM compiles that to
fcvt x3, fmul d, fadd d, fcvt s, fcvt d (7 instructions with the widenings). When all three
operands hold single-precision values, `crate::fmas` (one `fmadd s`) computes what the guest
CPU computes for these instructions: the exact a*b+c rounded once to single.

Operands are NOT always single: Metrowerks converts integers to float with
`lfd (0x43300000_xxxxxxxx); fsubs`, feeding doubles to a single op. So an op is rewritten only
when, earlier in the same basic block (match arm), each operand register was last written by an
instruction that produces a single-precision value (lfs, psq_l, another single op, or a copy of
such a register). Anything else (lfd, double ops, call results, values from other blocks)
leaves the op untouched. Plain single add/sub/mul/div are left alone: LLVM already narrows
those to f32 when it can see that the operands are widened singles.

Edits files in place; idempotent. tools/f32_ops.py --revert undoes it.
"""
import glob, re, sys

args = [a for a in sys.argv[1:] if not a.startswith("--")]
gen = args[0] if args else "crates/game/src/gen"
revert = "--revert" in sys.argv

R = r"(?:f|ps)\d+"
N = r"[abc][01]"
arm_re = re.compile(r"^\s*(?:0x[0-9A-F]{8} => \{|pub fn fn_[0-9A-F]{8}\(|_ => )")
# `{ let v = EXPR; fN = v; psN = v; }` (scalar single ops, lfs)
scalar_re = re.compile(rf"^(\s*\{{ (?:let ea = [^;]*; )?let v = )(.*?)(; (f\d+) = v; (ps\d+) = v; \}})\s*$")
# `{ let (a0, a1, ...) = (f2, ps2, ...); fN = E0; psN = E1; }` (paired single ops)
paired_re = re.compile(rf"^(\s*\{{ let \(([a-c][01](?:, [a-c][01])*)\) = \(({R}(?:, {R})*)\); )(f\d+) = (.*?); (ps\d+) = (.*?); \}}\s*$")
psq_re = re.compile(rf"\((f\d+), (ps\d+)\) = c\.psq_l_val\(")
copy_re = re.compile(rf"^\s*(f\d+|ps\d+) = ((?:f|ps)\d+);\s*$")
assign_re = re.compile(rf"(?<![\w.\[])((?:f|ps)\d+) = ")

madd = [  # (pattern on operand names, builder)
    (re.compile(rf"^\(-\(({R}|{N}) \* ({R}|{N}) - ({R}|{N})\)\) as f32 as f64$"), lambda a, b, c: f"(-crate::fmas({a}, {b}, -{c}))"),
    (re.compile(rf"^\(-\(({R}|{N}) \* ({R}|{N}) \+ ({R}|{N})\)\) as f32 as f64$"), lambda a, b, c: f"(-crate::fmas({a}, {b}, {c}))"),
    (re.compile(rf"^\(({R}|{N}) \* ({R}|{N}) \+ ({R}|{N})\) as f32 as f64$"), lambda a, b, c: f"crate::fmas({a}, {b}, {c})"),
    (re.compile(rf"^\(({R}|{N}) \* ({R}|{N}) - ({R}|{N})\) as f32 as f64$"), lambda a, b, c: f"crate::fmas({a}, {b}, -{c})"),
]
unmadd = [
    (re.compile(rf"\(-crate::fmas\(({R}|{N}), ({R}|{N}), -({R}|{N})\)\)"), r"(-(\1 * \2 - \3)) as f32 as f64"),
    (re.compile(rf"\(-crate::fmas\(({R}|{N}), ({R}|{N}), ({R}|{N})\)\)"), r"(-(\1 * \2 + \3)) as f32 as f64"),
    (re.compile(rf"crate::fmas\(({R}|{N}), ({R}|{N}), -({R}|{N})\)"), r"(\1 * \2 - \3) as f32 as f64"),
    (re.compile(rf"crate::fmas\(({R}|{N}), ({R}|{N}), ({R}|{N})\)"), r"(\1 * \2 + \3) as f32 as f64"),
]

def is_single_expr(e):
    return e.endswith(" as f32 as f64") or e.startswith("crate::fmas(") or e.startswith("(-crate::fmas(") \
        or (e.startswith("f32::from_bits(") and e.endswith(") as f64"))

def try_madd(expr, known, alias):
    for pat, build in madd:
        m = pat.match(expr)
        if m:
            regs = [alias.get(x, x) for x in m.groups()]
            if all(r in known for r in regs):
                return build(*m.groups())
            return None
    return None

stats = {"rewritten": 0, "candidates": 0}

def process(src):
    out = []
    known = set()
    for line in src.split("\n"):
        if arm_re.match(line):
            known = set()
            out.append(line)
            continue
        m = scalar_re.match(line)
        if m:
            head, expr, tail, fd, pd = m.groups()
            if " * " in expr and (" + " in expr or " - " in expr) and expr.endswith("as f32 as f64"):
                stats["candidates"] += 1
                new = try_madd(expr, known, {})
                if new:
                    stats["rewritten"] += 1
                    expr = new
                    line = f"{head}{expr}{tail}"
            known.discard(fd); known.discard(pd)
            if is_single_expr(expr):
                known.add(fd); known.add(pd)
            out.append(line)
            continue
        m = paired_re.match(line)
        if m:
            head, names, regs, fd, e0, pd, e1 = m.groups()
            alias = dict(zip(names.split(", "), regs.split(", ")))
            for i, e in ((0, e0), (1, e1)):
                if " * " in e and (" + " in e or " - " in e):
                    stats["candidates"] += 1
                    new = try_madd(e, known, alias)
                    if new:
                        stats["rewritten"] += 1
                        if i == 0: e0 = new
                        else: e1 = new
            line = f"{head}{fd} = {e0}; {pd} = {e1}; }}"
            known.discard(fd); known.discard(pd)
            if is_single_expr(e0): known.add(fd)
            if is_single_expr(e1): known.add(pd)
            out.append(line)
            continue
        m = copy_re.match(line)
        if m:
            d, s = m.groups()
            if s in known: known.add(d)
            else: known.discard(d)
            out.append(line)
            continue
        m = psq_re.search(line)
        for r in assign_re.findall(line):
            known.discard(r)
        if m and line.count(" = ") <= 3:
            known.add(m.group(1)); known.add(m.group(2))
        out.append(line)
    return "\n".join(out)

files = 0
for path in sorted(glob.glob(f"{gen}/m*.rs")):
    src = open(path).read()
    if revert:
        out = src
        for pat, rep in unmadd:
            out, n = pat.subn(rep, out)
            stats["rewritten"] += n
    else:
        out = process(src)
    if out != src:
        open(path, "w").write(out)
        files += 1
print(f"{'reverted' if revert else 'rewrote'} {stats['rewritten']} of {stats['candidates']} multiply-adds in {files} files")
