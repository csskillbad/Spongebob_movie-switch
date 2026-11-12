#!/usr/bin/env python3
"""Adds function-level profiler hooks (runtime::prof) to the generated code in crates/game/src/gen.

Edits the existing files in place (so hand edits in the generated code survive): each function
records its own address on entry and again after every non-tail call it makes returns. The hooks
cost nothing when runtime::PROFILE_FN is false. Idempotent; undo with `git checkout crates/game/src/gen`.
"""
import glob, re, sys

gen = sys.argv[1] if len(sys.argv) > 1 else "crates/game/src/gen"
fn_re = re.compile(r"^pub fn fn_([0-9A-F]{8})\(c: &mut Ctx\) \{$")
call_re = re.compile(r"(?<!return )((?:fn_[0-9A-F]{8}\(c\)|c\.call\(t\));)(?! runtime::prof)")
files = sites = funcs = 0
for path in sorted(glob.glob(f"{gen}/m*.rs")):
    out, cur, changed = [], None, False
    for line in open(path):
        m = fn_re.match(line.rstrip("\n"))
        if m:
            cur = m.group(1)
            out.append(line)
            out.append(f"    runtime::prof(0x{cur});\n")
            funcs += 1; changed = True
            continue
        if line.startswith("    runtime::prof(0x") and out and fn_re.match(out[-1].rstrip("\n")):
            out.pop(); funcs -= 1  # already instrumented: drop the duplicate we just added
        if cur and ("(c);" in line or "c.call(t);" in line):
            new, n = call_re.subn(lambda mm: f"{mm.group(1)} runtime::prof(0x{cur});", line)
            if n:
                line, sites, changed = new, sites + n, True
        out.append(line)
    if changed:
        open(path, "w").write("".join(out)); files += 1
print(f"instrumented {funcs} functions, {sites} call sites in {files} files")
