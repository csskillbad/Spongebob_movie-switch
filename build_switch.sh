#!/usr/bin/env bash
# ==============================================================================
# The SpongeBob SquarePants Movie - Nintendo Switch Build Script
# ==============================================================================
# Builds:
#   1. Rust static library for aarch64-unknown-none (Cortex-A57 target)
#   2. Native Switch executable + NRO with devkitA64 & OpenGL 4.3 runtime
#   3. Standalone installable NSP package (Title ID: 0100780012340000)
# ==============================================================================

set -eo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "=================================================================="
echo " Building The SpongeBob SquarePants Movie for Nintendo Switch"
echo "=================================================================="

# 1. Detect Container Engine (podman or docker)
CONTAINER_CMD=""
if command -v podman &>/dev/null; then
    CONTAINER_CMD="podman"
elif command -v docker &>/dev/null; then
    CONTAINER_CMD="docker"
else
    echo "[-] Error: Neither podman nor docker was found in PATH."
    echo "    Please install podman or docker to run the devkitA64 toolchain."
    exit 1
fi
echo "[+] Using container runtime: $CONTAINER_CMD"

# 2. Check Rust & cargo
if ! command -v cargo &>/dev/null; then
    echo "[-] Error: cargo is not installed or not in PATH."
    exit 1
fi

# Ensure output directories exist
mkdir -p target/switch/nsp
TMP_BUILD="/tmp/switch-build-$$"
mkdir -p "$TMP_BUILD"
trap 'rm -rf "$TMP_BUILD"' EXIT

# 3. Build Rust tssm-switch static library
echo ""
echo "=== [1/3] Building Rust tssm-switch (aarch64-unknown-none, Cortex-A57) ==="
RUSTC_BOOTSTRAP=1 RUSTFLAGS="-C relocation-model=pic -C target-cpu=cortex-a57 -C target-feature=-strict-align -C llvm-args=-jump-threading-across-loop-headers" \
cargo build -p tssm-switch ${TSSM_FEATURES:+--features $TSSM_FEATURES} \
    --target aarch64-unknown-none \
    --release \
    -Z build-std=core,alloc

LIB_RUST_PATH="target/aarch64-unknown-none/release/libtssm_switch.a"
if [ ! -f "$LIB_RUST_PATH" ]; then
    echo "[-] Error: Rust static library not found at $LIB_RUST_PATH"
    exit 1
fi
echo "[+] Compiled $LIB_RUST_PATH"

# 4. Compile C/C++ runtime & NEON assembly, link NRO
echo ""
echo "=== [2/3] Compiling C/C++/NEON runtime & Linking NRO (devkitA64) ==="
cp "$LIB_RUST_PATH" "$TMP_BUILD/libtssm_switch.a"
cp tools/switch/main.c \
   tools/switch/gl_backend.c \
   tools/switch/gl_backend.h \
   tools/switch/switch_neon.s \
   tools/switch/switch_accel.cpp \
   tools/switch/Makefile \
   tools/switch/icon.jpg \
   tools/switch/patch_armv9.py \
   "$TMP_BUILD/"

$CONTAINER_CMD run --rm \
    -v "$TMP_BUILD":/work:Z \
    -w /work \
    docker.io/devkitpro/devkita64:latest \
    make -C /work clean all LIB_RUST=/work/libtssm_switch.a

# Publish fresh artifacts where tools/make_nsp.sh picks them up (never a stale /tmp copy)
cp "$TMP_BUILD/main" target/switch/main
cp "$TMP_BUILD/tssm.elf" target/switch/tssm.elf

if [ -f "$TMP_BUILD/tssm.nro" ]; then
    cp "$TMP_BUILD/tssm.nro" target/switch/tssm.nro
    echo "[+] Generated Homebrew NRO: target/switch/tssm.nro"
else
    echo "[-] Error: Failed to generate tssm.nro"
    exit 1
fi

# 5. Package Standalone NSP
echo ""
echo "=== [3/3] Packaging Standalone All-in-One NSP ==="
if [ -f "tools/make_nsp.sh" ]; then
    bash tools/make_nsp.sh
else
    echo "[!] Warning: tools/make_nsp.sh not found, skipping NSP generation."
fi

echo ""
echo "=================================================================="
echo " BUILD SUCCESSFUL!"
if [ -f "target/switch/nsp/0100780012340000.nsp" ]; then
    echo " Standalone NSP : target/switch/nsp/0100780012340000.nsp"
fi
if [ -f "target/switch/tssm.nro" ]; then
    echo " Homebrew NRO   : target/switch/tssm.nro"
fi
echo "=================================================================="
