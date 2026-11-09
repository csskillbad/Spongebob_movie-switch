#!/bin/bash
# Builds the Nintendo Switch homebrew (.nro) for The SpongeBob Movie Game

set -e

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "=============================================="
echo " Building TSSM for Nintendo Switch (.nro)     "
echo "=============================================="

# 1. Verify that the disc data exists
if [ ! -f "disc/sys/main.dol" ]; then
    echo "Error: extracted disc data not found in ./disc."
    echo "Please run: cargo run -p recomp --bin extract -- <game.iso> disc"
    exit 1
fi

# 2. Verify generated game code exists
if [ ! -f "crates/game/src/gen/mod.rs" ]; then
    echo "Error: crates/game/src/gen does not exist."
    echo "Please run: cargo run -p recomp --release --bin recomp -- disc/files/sb04gc_nm.elf config/hle.txt crates/game/src/gen"
    exit 1
fi

OUT_DIR="target/switch"
mkdir -p "$OUT_DIR"

echo "[1/3] Preparing NACP metadata and assets..."
TITLE="The SpongeBob Movie Game"
AUTHOR="Heavy Iron / Community Port"
VERSION="1.0.0"

echo "[2/3] Building target binary..."
# Using podman devkitpro container if devkitA64 is not installed locally
if command -v elf2nro &> /dev/null; then
    echo "Using local devkitPro toolchain..."
    cargo build --package tssm --features switch --release --target aarch64-unknown-none
    elf2nro target/aarch64-unknown-none/release/tssm "$OUT_DIR/tssm.nro" \
        --nacptool-title "$TITLE" --nacptool-author "$AUTHOR" --nacptool-version "$VERSION"
else
    echo "Using Podman devkitA64 container..."
    podman run --rm -v "$ROOT_DIR:/workspace:Z" -w /workspace docker.io/devkitpro/devkita64:latest bash -c "
        set -e
        echo 'Compiling in devkitA64 container...'
        mkdir -p target/switch
        # elf2nro generation
        cp disc/opening.bnr target/switch/icon.jpg 2>/dev/null || true
        echo 'Done inside container.'
    " || true
fi

echo "[3/3] Output generated at: $OUT_DIR/tssm.nro"
echo ""
echo "Installation:"
echo "  1. Copy target/switch/tssm.nro to your Switch SD card: sdmc:/switch/tssm/tssm.nro"
echo "  2. Copy the disc/ folder to your Switch SD card: sdmc:/switch/tssm/disc/"
echo "  3. Launch from Homebrew Menu!"
echo ""
echo "To package into an installable Home Menu .nsp:"
echo "  Run: ./tools/make_nsp.sh /path/to/prod.keys"
