#!/bin/bash
# Builds an installable Nintendo Switch .nsp package for The SpongeBob Movie Game
# Uses hacbrewpack with your Ryujinx prod.keys

set -e

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

KEYS_FILE="${1:-}"
if [ -z "$KEYS_FILE" ]; then
    for cand in \
        "$ROOT_DIR/prod.keys" \
        "$ROOT_DIR/keys.dat" \
        "$HOME/.config/Ryujinx/system/prod.keys" \
        "$HOME/.switch/prod.keys" \
        "$HOME/.local/share/yuzu/keys/prod.keys" \
        "/home/vavaci/.config/Ryujinx/system/prod.keys"; do
        if [ -f "$cand" ]; then
            KEYS_FILE="$cand"
            break
        fi
    done
fi
TITLE_ID="0100780012340000"
TITLE_NAME="The SpongeBob Movie Game"
PUBLISHER="Heavy Iron / Recomp"
VERSION="1.0.0"

echo "=========================================================="
echo " Packaging The SpongeBob Movie Game for Nintendo Switch    "
echo " Format: Installable Home Menu Package (.nsp)             "
echo " Title ID: $TITLE_ID                                      "
echo "=========================================================="

if [ -z "$KEYS_FILE" ] || [ ! -f "$KEYS_FILE" ]; then
    if [ -x "$ROOT_DIR/tssm-builder" ]; then
        echo "[!] No prod.keys found; using native Rust builder (./tssm-builder --build-all)..."
        "$ROOT_DIR/tssm-builder" --build-all
        exit 0
    fi
    echo "Error: prod.keys not found in standard locations and ./tssm-builder not executable."
    exit 1
fi

echo "[1/5] Sanitizing keyset..."
SANITIZED_KEYS="/tmp/tssm_keys.dat"
python3 -c "
with open('$KEYS_FILE') as f, open('$SANITIZED_KEYS', 'w') as out:
    for line in f:
        trimmed = line.strip()
        if '=' in trimmed and not trimmed.startswith('#'):
            k, v = trimmed.split('=', 1)
            k, v = k.strip(), v.strip()
            if len(v) == 34 and v.endswith('00'):
                v = v[:-2]
            out.write(f'{k} = {v}\n')
        else:
            out.write(line)
"

STAGE_DIR="target/switch/staging"
EXEFS_DIR="$STAGE_DIR/exefs"
CONTROL_DIR="$STAGE_DIR/control"
NSP_DIR="target/switch/nsp"

mkdir -p "$EXEFS_DIR" "$CONTROL_DIR" "$NSP_DIR"

echo "[2/5] Generating NPDM process descriptor (npdmtool)..."
cat << 'EOF' > "$STAGE_DIR/npdm.json"
{
    "name": "tssm",
    "title_id": "0x0100780012340000",
    "program_id": "0x0100780012340000",
    "title_id_range_min": "0x0100000000010000",
    "program_id_range_min": "0x0100000000010000",
    "title_id_range_max": "0x01ffffffffffffff",
    "program_id_range_max": "0x01ffffffffffffff",
    "main_thread_stack_size": "0x00100000",
    "main_thread_priority": 44,
    "default_cpu_id": 0,
    "process_category": "0x0",
    "version": "0x00000000",
    "system_resource_size": "0x0",
    "optimize_memory_allocation": true,
    "disable_device_address_space_merge": false,
    "enable_alias_region_extra_size": false,
    "prevent_code_reads": false,
    "signature_key_generation": 1,
    "is_64_bit": true,
    "address_space_type": 3,
    "is_retail": true,
    "pool_partition": 0,
    "filesystem_access": {
        "permissions": "0xffffffffffffffff"
    },
    "service_access": [
        "acc:u0",
        "aoc:u",
        "apm",
        "appletOE",
        "appletAE",
        "audin:u",
        "audout:u",
        "audren:u",
        "banana",
        "bcat:u",
        "bsd:u",
        "bsdcfg",
        "caps:su",
        "caps:u",
        "csrng",
        "ectx:aw",
        "fatal:u",
        "friend:u",
        "fsp-srv",
        "hid",
        "hid:sys",
        "htc",
        "htc:tenv",
        "htcs",
        "hwopus",
        "irs",
        "ldn:u",
        "ldr:ro",
        "lm",
        "mii:u",
        "mm:u",
        "mnpp:app",
        "nfc:mf:u",
        "nfc:user",
        "nfp:user",
        "ngc:u",
        "ngct:u",
        "nifm:u",
        "nim:eca",
        "notif:a",
        "ns:am",
        "ns:ro",
        "nsd:u",
        "ntc",
        "nvdrv",
        "pcm",
        "pctl",
        "pctl:a",
        "pl:u",
        "prepo:u",
        "set",
        "set:sys",
        "sfdnsres",
        "ssl",
        "time:r",
        "time:u",
        "vi:m",
        "vi:u"
    ],
    "service_host": [],
    "kernel_capabilities": [
        {
            "type": "kernel_flags",
            "value": {
                "highest_thread_priority": 59,
                "lowest_thread_priority": 28,
                "lowest_cpu_id": 0,
                "highest_cpu_id": 2
            }
        },
        {
            "type": "syscalls",
            "value": {
                "svcSetHeapSize": "0x01",
                "svcSetMemoryPermission": "0x02",
                "svcSetMemoryAttribute": "0x03",
                "svcMapMemory": "0x04",
                "svcUnmapMemory": "0x05",
                "svcQueryMemory": "0x06",
                "svcExitProcess": "0x07",
                "svcCreateThread": "0x08",
                "svcStartThread": "0x09",
                "svcExitThread": "0x0a",
                "svcSleepThread": "0x0b",
                "svcGetThreadPriority": "0x0c",
                "svcSetThreadPriority": "0x0d",
                "svcGetThreadCoreMask": "0x0e",
                "svcSetThreadCoreMask": "0x0f",
                "svcGetCurrentProcessorNumber": "0x10",
                "svcSignalEvent": "0x11",
                "svcClearEvent": "0x12",
                "svcMapSharedMemory": "0x13",
                "svcUnmapSharedMemory": "0x14",
                "svcCreateTransferMemory": "0x15",
                "svcCloseHandle": "0x16",
                "svcResetSignal": "0x17",
                "svcWaitSynchronization": "0x18",
                "svcCancelSynchronization": "0x19",
                "svcArbitrateLock": "0x1a",
                "svcArbitrateUnlock": "0x1b",
                "svcWaitProcessWideKeyAtomic": "0x1c",
                "svcSignalProcessWideKey": "0x1d",
                "svcGetSystemTick": "0x1e",
                "svcConnectToNamedPort": "0x1f",
                "svcSendSyncRequestLight": "0x20",
                "svcSendSyncRequest": "0x21",
                "svcSendSyncRequestWithUserBuffer": "0x22",
                "svcSendAsyncRequestWithUserBuffer": "0x23",
                "svcGetProcessId": "0x24",
                "svcGetThreadId": "0x25",
                "svcBreak": "0x26",
                "svcOutputDebugString": "0x27",
                "svcReturnFromException": "0x28",
                "svcGetInfo": "0x29",
                "svcFlushEntireDataCache": "0x2a",
                "svcFlushDataCache": "0x2b",
                "svcMapPhysicalMemory": "0x2c",
                "svcUnmapPhysicalMemory": "0x2d",
                "svcGetLastThreadInfo": "0x2f",
                "svcSetThreadActivity": "0x32",
                "svcGetThreadContext3": "0x33",
                "svcWaitForAddress": "0x34",
                "svcSignalToAddress": "0x35",
                "svcSynchronizePreemptionState": "0x36",
                "svcCreateSession": "0x40",
                "svcAcceptSession": "0x41",
                "svcReplyAndReceiveLight": "0x42",
                "svcReplyAndReceive": "0x43",
                "svcReplyAndReceiveWithUserBuffer": "0x44",
                "svcCreateEvent": "0x45",
                "svcCreateSharedMemory": "0x50",
                "svcMapTransferMemory": "0x51",
                "svcUnmapTransferMemory": "0x52",
                "svcCreateInterruptEvent": "0x53",
                "svcQueryPhysicalAddress": "0x54",
                "svcQueryIoMapping": "0x55",
                "svcCreateDeviceAddressSpace": "0x56",
                "svcAttachDeviceAddressSpace": "0x57",
                "svcDetachDeviceAddressSpace": "0x58",
                "svcMapDeviceAddressSpaceByForce": "0x59",
                "svcMapDeviceAddressSpaceAligned": "0x5a",
                "svcMapDeviceAddressSpace": "0x5b",
                "svcUnmapDeviceAddressSpace": "0x5c",
                "svcInvalidateProcessDataCache": "0x5d",
                "svcStoreProcessDataCache": "0x5e",
                "svcFlushProcessDataCache": "0x5f",
                "svcSetProcessMemoryPermission": "0x73",
                "svcMapProcessMemory": "0x74",
                "svcUnmapProcessMemory": "0x75",
                "svcQueryProcessMemory": "0x76",
                "svcMapProcessCodeMemory": "0x77",
                "svcUnmapProcessCodeMemory": "0x78"
            }
        },
        {
            "type": "application_type",
            "value": 1
        },
        {
            "type": "min_kernel_version",
            "value": "0x0091"
        },
        {
            "type": "handle_table_size",
            "value": 512
        },
        {
            "type": "debug_flags",
            "value": {
                "allow_debug": true,
                "force_debug": false
            }
        }
    ]
}
EOF

npdmtool "$STAGE_DIR/npdm.json" "$EXEFS_DIR/main.npdm"

echo "[3/5] Generating NACP control metadata (nacptool)..."
nacptool --create "$TITLE_NAME" "$PUBLISHER" "$VERSION" "$CONTROL_DIR/control.nacp" --titleid="$TITLE_ID"

if [ -f "$ROOT_DIR/tools/switch/icon.jpg" ]; then
    cp "$ROOT_DIR/tools/switch/icon.jpg" "$CONTROL_DIR/icon_AmericanEnglish.dat"
elif [ -f "/tmp/default_icon.jpg" ]; then
    cp /tmp/default_icon.jpg "$CONTROL_DIR/icon_AmericanEnglish.dat"
else
    echo "Warning: No icon found!"
fi

echo "[4/5] Preparing main executable..."
if [ -f "target/switch/main" ]; then
    cp target/switch/main "$EXEFS_DIR/main"
elif [ -f "target/switch/main" ]; then
    cp target/switch/main "$EXEFS_DIR/main"
else
    echo "Error: valid ARM64 main NSO not found!"
    exit 1
fi

ROMFS_DIR="$STAGE_DIR/romfs"
mkdir -p "$ROMFS_DIR/config"
# No scene.txt / autodemo.flag in the package: the game boots through its real front end
# (logos, title screen, main menu). Both remain available as SD-card files for testing.
rm -f "$ROMFS_DIR/config/scene.txt" "$ROMFS_DIR/config/autodemo.flag"
echo "1" > "$ROMFS_DIR/config/fps_counter.txt"
rm -f "$ROMFS_DIR/config/scale.txt"

if [ ! -f "$ROMFS_DIR/boot.txt" ]; then
    echo "TSSM Switch RomFS Root" > "$ROMFS_DIR/boot.txt"
fi
if [ -d "$ROOT_DIR/disc/files" ]; then
    if [ ! -d "$ROMFS_DIR/files" ]; then
        echo "Staging disc/files into RomFS..."
        cp -r "$ROOT_DIR/disc/files" "$ROMFS_DIR/"
    else
        echo "Updating staged disc/files..."
        cp "$ROOT_DIR/disc/files/SB04.ini" "$ROMFS_DIR/files/SB04.ini"
    fi
fi

echo "[5/6] Packaging standalone all-in-one .nro (with embedded RomFS)..."
if [ -f "target/switch/tssm.elf" ]; then
    elf2nro target/switch/tssm.elf "$ROOT_DIR/target/switch/tssm.nro" \
        --icon="$CONTROL_DIR/icon_AmericanEnglish.dat" \
        --nacp="$CONTROL_DIR/control.nacp" \
        --romfsdir="$ROMFS_DIR"
fi

echo "[6/6] Packaging NSP container (hacbrewpack)..."
hacbrewpack \
    -k "$SANITIZED_KEYS" \
    --titleid "$TITLE_ID" \
    --titlename "$TITLE_NAME" \
    --titlepublisher "$PUBLISHER" \
    --romfsdir "$ROMFS_DIR" \
    --nologo \
    --controldir "$CONTROL_DIR" \
    --exefsdir "$EXEFS_DIR" \
    --nspdir "$NSP_DIR"

OUTPUT_NSP="$NSP_DIR/$TITLE_ID.nsp"

# Keep RomFS-free GUI builder templates (tools/switch/tssm_engine.nro & tssm_nsp_template.bin) in sync
if [ -f "$ROOT_DIR/target/switch/tssm.nro" ] && [ -f "$OUTPUT_NSP" ]; then
    python3 -c "
import struct

nro_path = '$ROOT_DIR/target/switch/tssm.nro'
nsp_path = '$OUTPUT_NSP'
eng_out  = '$ROOT_DIR/tools/switch/tssm_engine.nro'
tpl_out  = '$ROOT_DIR/tools/switch/tssm_nsp_template.bin'

with open(nro_path, 'rb') as f:
    f.seek(0x18)
    nro_sz = struct.unpack('<I', f.read(4))[0]
    f.seek(nro_sz)
    aset = struct.unpack('<4sIQQQQQQ', f.read(56))
    assert aset[0] == b'ASET', 'Invalid ASET header in tssm.nro'
    eng_len  = nro_sz + aset[6]
    romfs_sz = aset[7]
    f.seek(0)
    with open(eng_out, 'wb') as out:
        out.write(f.read(eng_len))

with open(nsp_path, 'rb') as f:
    f.seek(0, 2)
    nsp_total = f.tell()
    f.seek(0)
    hdr = f.read(216)
    assert hdr[:4] == b'PFS0', 'Invalid PFS0 header in NSP'
    c_sz = struct.unpack('<Q', hdr[16 + 24 + 8 : 16 + 24 + 16])[0]
    m_sz = struct.unpack('<Q', hdr[16 + 48 + 8 : 16 + 48 + 16])[0]
    tail_pad   = ((romfs_sz + 0x3FFF) & ~0x3FFF) - romfs_sz
    suffix_len = tail_pad + c_sz + m_sz
    prefix_len = nsp_total - romfs_sz - suffix_len
    assert prefix_len > 216 and (prefix_len - 216) % 16 == 0, 'Unaligned NSP RomFS offset'
    f.seek(0)
    with open(tpl_out, 'wb') as out:
        out.write(f.read(prefix_len))
        f.seek(prefix_len + romfs_sz)
        out.write(f.read(suffix_len))
print(f'[templates] Synced tssm_engine.nro ({eng_len} B) and tssm_nsp_template.bin (prefix={prefix_len}, suffix={suffix_len})')
"
fi

# Clear stale Ryujinx PPTC translation cache for this Title ID so updated NSO offsets don't collide
rm -rf "$HOME/.config/Ryujinx/games/$TITLE_ID/cache/cpu"/* 2>/dev/null || true

echo ""
echo "=========================================================="
echo " BUILD SUCCESSFUL!"
echo " Package location: $OUTPUT_NSP"
echo " File size: $(ls -lh "$OUTPUT_NSP" | awk '{print $5}')"
echo "=========================================================="
