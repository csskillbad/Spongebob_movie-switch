#!/usr/bin/env python3
"""Drops redundant reloads of callee-saved registers after direct calls in crates/game/src/gen.

The PowerPC EABI requires a callee to preserve r14-r31 and f14-f31 (the double / ps0 half), so
after `fn_X(c);` the caller's local copies are still correct and re-reading them from `c.r` /
`c.f` is wasted work (about 10 loads per call site). Paired-single halves (ps*), CR fields, r1,
r2, r13 and every volatile register are still reloaded. Calls to helpers that deliberately change
callee-saved registers (_restgpr_*, _restfpr_*, __restore_*, OSLoadContext, exception unwinding)
keep their full reloads, as do indirect calls (c.call). Edits files in place; idempotent; undo
with `git checkout crates/game/src/gen`.
"""
import glob, re, sys

gen = sys.argv[1] if len(sys.argv) > 1 else "crates/game/src/gen"
names = dict(re.findall(r'\(0x([0-9A-F]{8}), fn_[0-9A-F]{8}, "([^"]*)"\)', open(f"{gen}/mod.rs").read()))
unsafe_re = re.compile(r"^(_restgpr_|_restfpr_|__restore_|OSLoadContext|ExPPC_|__throw|__unwind|__end__catch|longjmp)")
unsafe = {a for a, n in names.items() if unsafe_re.match(n)}

site_re = re.compile(r"fn_([0-9A-F]{8})\(c\);((?: runtime::prof\(0x[0-9A-F]{8}\);)?)((?: [a-z]+[0-9]* = c\.[a-z0-9]+(?:\[[0-9]+\])?;)+)")
drop_re = re.compile(r" (?:r(1[4-9]|2[0-9]|3[01]) = c\.r\[\1\]|f(1[4-9]|2[0-9]|3[01]) = c\.f\[\2\]);")
files = sites = dropped = kept = 0

def fix(m):
    global sites, dropped, kept
    if m.group(1) in unsafe:
        kept += 1
        return m.group(0)
    reloads, n = drop_re.subn("", m.group(3))
    sites += 1; dropped += n
    return f"fn_{m.group(1)}(c);{m.group(2)}{reloads}"

for path in sorted(glob.glob(f"{gen}/m*.rs")):
    src = open(path).read()
    out = site_re.sub(fix, src)
    if out != src:
        open(path, "w").write(out); files += 1
print(f"{len(unsafe)} unsafe callees; trimmed {dropped} reloads at {sites} call sites in {files} files; kept {kept} unsafe sites intact")
