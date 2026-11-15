# The SpongeBob SquarePants Movie - Nintendo Switch Port

A native, high-performance static recompilation of **The SpongeBob SquarePants Movie** (GameCube USA, `GGVE78` revision 1) for the **Nintendo Switch**, written in Rust, C++, and AArch64 NEON assembly.

Runs natively on bare-metal ARM64 (Cortex-A57) with direct hardware graphics acceleration via modern OpenGL 4.3 — **no emulator involved**.

---

## Features

- **Native 60 FPS Performance**: Runs natively on the Nintendo Switch hardware, tuned specifically for the ARM Cortex-A57 CPU architecture with LLVM ThinLTO.
- **AArch64 NEON SIMD Acceleration**:
  - Hand-crafted assembly routines (`switch_neon.s`) for RenderWare skeletal bone blending (`_rpSkinMatrixBlendUpdateASM`), keeping transform matrices cached in vector registers with zero stack spills.
  - Dual-pipeline unrolled vertex and vector transforms (`RwV3dTransformPoints`, `RwV3dTransformVectors`).
  - Single-cycle bit-scan asset lookups (`__builtin_ctz`) replacing 32-step software loops.
  - Zero-allocation direct-register fast-path decoding for matrix-indexed and skinned character models.
- **Dynamic 1:1 Native Resolution**:
  - **Handheld Mode**: Native **720p** (1280x720 active pixel match).
  - **Docked Mode**: Native **1080p** (1920x1080 active pixel match).
- **Controls & Input**:
  - Full support for Joy-Cons (handheld & detached) and Nintendo Switch Pro Controllers.
  - Hardware radial stick deadzones to prevent drift.
- **Audio & Video**:
  - Real-time 64-voice DSP/AX software audio mixer with native sample rate conversion.
  - Full Bink video playback for cutscenes.
- **Dual Packaging**:
  - **Standalone NSP**: All-in-one installable NSP (`Title ID: 0100780012340000`) installable directly to the Home Menu.
  - **Homebrew NRO**: Standalone Homebrew Menu executable (`tssm.nro`).

---

## Easy Build Instructions

### Prerequisites

- **Linux** (x86_64 or ARM64)
- **Rust toolchain** (Nightly or Stable with `build-std` support, `rustup target add aarch64-unknown-none`)
- **Podman** or **Docker** (for automated devkitA64 container toolchain)
- **hacbrewpack** & **hactool** (for NSP packaging; already configured in `tools/`)

### 1-Step Build

Simply run the unified build script:

```bash
chmod +x build_switch.sh
./build_switch.sh
```

The script will automatically:
1. Compile the Rust static library (`libtssm_switch.a`) optimized for Cortex-A57.
2. Compile C/C++ runtime and AArch64 NEON assembly via devkitA64.
3. Link the Switch Homebrew executable (`target/switch/tssm.nro`).
4. Package the standalone installable NSP (`target/switch/nsp/0100780012340000.nsp`).

---

## Installation

### Option A: Install Standalone NSP (Recommended)
1. Copy `target/switch/nsp/0100780012340000.nsp` to your Switch SD card or install over USB.
2. Install with **DBI**, **Goldleaf**, or **Awoo Installer**.
3. Launch directly from the Switch Home Menu!

### Option B: Homebrew NRO
1. Copy `target/switch/tssm.nro` to `sdmc:/switch/tssm/tssm.nro`.
2. Ensure game assets (`files/`) are placed in `sdmc:/switch/tssm/files/`.
3. Launch via the Homebrew Menu (holding R while launching any game for full RAM access).

---

## Controls

| Switch Button | Action |
|---|---|
| **Left Stick** | SpongeBob / Patrick Movement |
| **Right Stick** | 360° Camera Control |
| **A** | Jump / Double Jump / Select |
| **B** | Spin Attack / Cancel |
| **X** | Bowling Ball / Special Move |
| **Y** | Bash / Interaction |
| **ZL / ZR** | Sneak / Crouch / Cruise Bubble |
| **L / R** | Center Camera |
| **Plus (+)** | Pause Menu |

---

## Repository Layout

| Directory | Description |
|---|---|
| `crates/switch` | Nintendo Switch host implementation (audio, input, OpenGL 4.3 renderer, filesystem) |
| `crates/runtime` | GameCube CPU context, memory mappings, and decompilation runtime |
| `crates/game` | Recompiled GameCube game functions and logic |
| `crates/recomp` | PowerPC binary static recompiler and ISO unpacker |
| `tools/switch` | Switch C/C++ runtime, AArch64 NEON assembly (`switch_neon.s`), and Makefile |
| `build_switch.sh` | One-click automated build script for Nintendo Switch |

---

## Legal

This project is not affiliated with, endorsed by, or connected to Nickelodeon, THQ, Heavy Iron Studios, or Nintendo.
SpongeBob SquarePants is a trademark of Viacom International Inc.
No proprietary game code, assets, audio, or textures are distributed in this repository. You must supply your own legally acquired copy of the original GameCube disc.
