#!/usr/bin/env bash
# ==============================================================================
# Builds the Pure x86_64 Assembly GUI Builders for Windows (.exe) & Linux (X11)
# ==============================================================================
set -eo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$ROOT_DIR"

echo "=== Building Pure x86_64 Assembly GUI Builders ==="

# 1. Windows x86_64 PE32+ GUI Executable (TSSM-Builder.exe)
if command -v x86_64-w64-mingw32-gcc &>/dev/null; then
    echo "[1/2] Assembling Windows Win32 GDI GUI -> TSSM-Builder.exe"
    x86_64-w64-mingw32-gcc -mwindows -O2 -s \
        tools/gui_builder/builder_win64.s \
        -o TSSM-Builder.exe \
        -luser32 -lgdi32 -lcomdlg32 -lshell32 -lkernel32
    echo "      [+] Built TSSM-Builder.exe ($(ls -lh TSSM-Builder.exe | awk '{print $5}'))"
else
    echo "      [!] x86_64-w64-mingw32-gcc not found; skipping Windows .exe rebuild"
fi

# 2. Linux x86_64 ELF64 X11 GUI Executable (tssm-builder)
if command -v gcc &>/dev/null; then
    echo "[2/2] Assembling Linux X11 GUI -> tssm-builder"
    X11_LIB="/usr/lib64/libX11.so.6"
    if [ ! -f "$X11_LIB" ]; then
        X11_LIB="-lX11"
    fi
    gcc -no-pie -O2 -s \
        tools/gui_builder/builder_linux64.s \
        "$X11_LIB" \
        -o tssm-builder
    echo "      [+] Built tssm-builder ($(ls -lh tssm-builder | awk '{print $5}'))"
fi

echo "=== Done! ==="
