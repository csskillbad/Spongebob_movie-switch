#!/usr/bin/env python3
import sys

def main():
    if len(sys.argv) < 2:
        print("Usage: patch_armv9.py <elf-file>")
        sys.exit(1)

    path = sys.argv[1]
    with open(path, "rb") as f:
        data = f.read()

    nop = b"\x1f\x20\x03\xd5"
    patterns = [
        (b"\x21\x25\x3b\xd5", "gcspr_el0"),
        (b"\x1f\x25\x03\xd5", "chkfeat x16"),
        (b"\x3f\x77\x2b\xd5", "gcspopm"),
        (b"\xae\xd0\x3b\xd5", "mrs tpidr2_el0"),
        (b"\xbf\xd0\x1b\xd5", "msr tpidr2_el0"),
    ]

    total_patched = 0
    for pat, name in patterns:
        count = data.count(pat)
        if count > 0:
            print(f"[patch_armv9] Replacing {count} instance(s) of ARMv9 '{name}' ({pat.hex()}) with NOP")
            data = data.replace(pat, nop)
            total_patched += count
        else:
            print(f"[patch_armv9] '{name}' not found (clean)")

    with open(path, "wb") as f:
        f.write(data)

    print(f"[patch_armv9] Patched {total_patched} ARMv9 instructions to NOP in {path}.")

if __name__ == "__main__":
    main()
