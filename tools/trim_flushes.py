#!/usr/bin/env python3
"""Drops redundant stores of callee-saved registers before direct calls in crates/game/src/gen.

Before `fn_X(c);` the generated code copies every register the caller has written into the
context (`c.r[k] = rk;`). Under the PowerPC EABI a callee never reads r14-r31 / f14-f31 (the
double half) as inputs: it only saves them to its stack frame and restores them before
returning, so whatever value the context holds is preserved untouched, and the caller keeps the
real value in its own local (its exit path stores it back into the context). Those stores are
therefore wasted, typically several per call site.

Kept as they were:
  * calls to helpers that do read callee-saved registers: _savegpr_*/_savefpr_*/__save_*
    (prologue helpers saving the CALLER's registers), the matching restore helpers, the OS
    context functions, setjmp/longjmp, and the C++ exception runtime (ExPPC_*, __throw...);
  * tail calls (`return fn_X(c)`), where the callee's return is the caller's return;
  * indirect calls (c.call), paired-single halves (ps*), CR fields, r0-r13 and f0-f13.

Pairs with tools/trim_reloads.py. Edits files in place; idempotent; undo with
`git checkout crates/game/src/gen`.
"""
import glob, re, sys

gen = sys.argv[1] if len(sys.argv) > 1 else "crates/game/src/gen"
names = dict(re.findall(r'\(0x([0-9A-F]{8}), fn_[0-9A-F]{8}, "([^"]*)"\)', open(f"{gen}/mod.rs").read()))
unsafe_re = re.compile(
    r"^(_savegpr_|_savefpr_|__save_|_restgpr_|_restfpr_|__restore_|OSSaveContext|OSLoadContext|"
    r"__OSSaveFPUContext|OSSaveFPUContext|__OSLoadFPUContext|OSSwitchFPUContext|OSDumpContext|"
    r"OSClearContext|OSInitContext|__OSContextInit|OSSetCurrentContext|ExPPC_|__throw|__unexpected|"
    r"__end__catch|setjmp|__setjmp|longjmp)"
)
unsafe = {a for a, n in names.items() if unsafe_re.match(n)}

# `{ c.lr = 0x...; <stores> fn_X(c);` -- a non-tail direct call (tail calls never set c.lr).
site_re = re.compile(r"(\{ c\.lr = 0x[0-9A-F]{8}; )((?:c\.[a-z0-9]+(?:\[[0-9]+\])? = [a-z]+[0-9]*; )*)fn_([0-9A-F]{8})\(c\);")
drop_re = re.compile(r"c\.(?:r\[(1[4-9]|2[0-9]|3[01])\] = r\1|f\[(1[4-9]|2[0-9]|3[01])\] = f\2); ")
files = sites = dropped = kept = 0

def fix(m):
    global sites, dropped, kept
    if m.group(3) in unsafe:
        kept += 1
        return m.group(0)
    stores, n = drop_re.subn("", m.group(2))
    sites += 1
    dropped += n
    return f"{m.group(1)}{stores}fn_{m.group(3)}(c);"

for path in sorted(glob.glob(f"{gen}/m*.rs")):
    src = open(path).read()
    out = site_re.sub(fix, src)
    if out != src:
        open(path, "w").write(out)
        files += 1
print(f"{len(unsafe)} unsafe callees; dropped {dropped} stores at {sites} call sites in {files} files; kept {kept} unsafe sites intact")
