#!/usr/bin/env bash
# ==============================================================================
# Builds the Native Rust Switch Builders for Windows (.exe) & Linux
# Output:
#   ./TSSM-Builder.exe  (Windows x64 Native Win32 GUI + Drag-and-Drop + CLI)
#   ./tssm-builder      (Linux x64 CLI + Zenity GUI)
# ==============================================================================
set -eo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILDER_DIR="$ROOT_DIR/tools/builder_rs"

echo "=== Building Native Rust Switch Builders (tools/builder_rs) ==="

cd "$BUILDER_DIR"

echo "[0/3] Running crypto & unit tests..."
cargo test --quiet

echo "[1/3] Building Linux x86_64 release binary -> tssm-builder"
cargo build --release
cp "$BUILDER_DIR/target/release/tssm-builder" "$ROOT_DIR/tssm-builder"
chmod +x "$ROOT_DIR/tssm-builder"
echo "      [+] Built tssm-builder ($(ls -lh "$ROOT_DIR/tssm-builder" | awk '{print $5}'))"

if command -v x86_64-w64-mingw32-gcc &>/dev/null; then
    echo "[2/3] Building Windows x86_64 GUI executable -> TSSM-Builder.exe"
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    RUSTFLAGS="-C target-feature=+crt-static" \
        cargo build --release --target x86_64-pc-windows-gnu
    cp "$BUILDER_DIR/target/x86_64-pc-windows-gnu/release/tssm-builder.exe" "$ROOT_DIR/TSSM-Builder.exe"
    echo "      [+] Built TSSM-Builder.exe ($(ls -lh "$ROOT_DIR/TSSM-Builder.exe" | awk '{print $5}'))"

    echo "[3/3] Verifying Windows PE imports (must contain ZERO MinGW runtime DLLs)..."
    IMPORTS="$(x86_64-w64-mingw32-objdump -p "$ROOT_DIR/TSSM-Builder.exe" | grep 'DLL Name:' || true)"
    echo "$IMPORTS"
    if echo "$IMPORTS" | grep -qiE 'libgcc|libwinpthread|libstdc\+\+'; then
        echo "ERROR: TSSM-Builder.exe depends on external MinGW DLLs!" >&2
        exit 1
    fi
    echo "      [+] Verified: 100% standalone Windows system DLL imports only!"
else
    echo "      [!] x86_64-w64-mingw32-gcc not found; skipping Windows .exe build"
fi

echo "=== Done! ==="
