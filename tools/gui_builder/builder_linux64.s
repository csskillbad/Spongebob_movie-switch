# ==============================================================================
# The SpongeBob SquarePants Movie - Native GUI Builder (Linux x86_64 Assembly)
# ==============================================================================
# Pure x86_64 assembly (GAS Intel syntax, System V AMD64 ABI) X11 GUI & CLI
# builder that natively constructs:
#   - target/switch/tssm.nro (747 MB All-in-One Homebrew NRO with RomFS)
#   - target/switch/nsp/0100780012340000.nsp (738 MB Installable NSP via AES-NI)
# directly from the user's GameCube ISO without Docker, devkitPro, or prod.keys.
# ==============================================================================

.intel_syntax noprefix
.text

# X11 & POSIX Constants
.equ KeyPress,            2
.equ ButtonPress,         4
.equ MotionNotify,        6
.equ Expose,              12
.equ ClientMessage,       33
.equ KeyPressMask,        (1 << 0)
.equ ButtonPressMask,     (1 << 2)
.equ PointerMotionMask,   (1 << 6)
.equ ExposureMask,        (1 << 15)
.equ StructureNotifyMask, (1 << 17)
.equ EVENT_MASK,          (ExposureMask | ButtonPressMask | PointerMotionMask | KeyPressMask | StructureNotifyMask)

.equ O_RDONLY,            0
.equ O_WRONLY_CREAT_TRUNC, 0x241        # O_WRONLY | O_CREAT | O_TRUNC
.equ SEEK_SET,            0

# X11 TrueColor RGB Palette (0x00RRGGBB)
.equ COL_BG_DARK,         0x000C1322   # Deep Bikini Bottom Navy
.equ COL_CARD_BG,         0x00152036   # Card Navy
.equ COL_CARD_BORDER,     0x0024385A   # Card Border
.equ COL_HEADER_BG,       0x00101A2D   # Header Navy
.equ COL_GOLD,            0x00FFC815   # SpongeBob Gold
.equ COL_GOLD_HOVER,      0x00FFD845   # Bright Gold Hover
.equ COL_CYAN,            0x0000B8F0   # Switch Neon Cyan
.equ COL_CYAN_HOVER,      0x0028CEFF   # Bright Cyan Hover
.equ COL_TEAL,            0x0014B896   # Seafoam Teal
.equ COL_TEAL_HOVER,      0x0028D4B0   # Bright Teal Hover
.equ COL_SLATE,           0x00283852   # Slate Button
.equ COL_SLATE_HOVER,     0x00364C6E   # Slate Hover
.equ COL_INPUT_BG,        0x00090F1B   # Dark Input Field
.equ COL_TEXT_WHITE,      0x00FAFAFA   # Crisp White
.equ COL_TEXT_MUTED,      0x0090A8C8   # Soft Ocean Gray-Blue
.equ COL_TEXT_DARK,       0x00081014   # Dark text on Gold/Cyan buttons
.equ COL_GREEN,           0x0030E070   # Ready / Success Green

# External libc & libX11 symbols
.extern XOpenDisplay
.extern XDefaultScreen
.extern XRootWindow
.extern XDefaultGC
.extern XCreateSimpleWindow
.extern XStoreName
.extern XSelectInput
.extern XMapWindow
.extern XInternAtom
.extern XSetWMProtocols
.extern XNextEvent
.extern XSetForeground
.extern XFillRectangle
.extern XDrawString
.extern XFlush
.extern XCloseDisplay
.extern strlen
.extern strcpy
.extern strcat
.extern strcmp
.extern access
.extern open
.extern read
.extern write
.extern lseek
.extern close
.extern mkdir
.extern popen
.extern fgets
.extern pclose
.extern fork
.extern execl
.extern puts
.extern _exit

# ==============================================================================
# Helper: aes_ctr_encrypt_buf(rdi = buf_ptr, rsi = byte_len)
# Hardware x86_64 AES-NI AES-128-CTR encryption for Switch NCA Section 1 (RomFS)
# ==============================================================================
aes_ctr_encrypt_buf:
    test rsi, rsi
    jz .Laes_done
    mov r9, qword ptr [rip + g_aes_ctr]
.Laes_loop:
    mov rax, r9
    bswap rax
    pxor xmm0, xmm0
    pinsrq xmm0, rax, 1
    inc r9

    pxor xmm0, xmmword ptr [rip + aes_round_keys + 0]
    aesenc xmm0, xmmword ptr [rip + aes_round_keys + 16]
    aesenc xmm0, xmmword ptr [rip + aes_round_keys + 32]
    aesenc xmm0, xmmword ptr [rip + aes_round_keys + 48]
    aesenc xmm0, xmmword ptr [rip + aes_round_keys + 64]
    aesenc xmm0, xmmword ptr [rip + aes_round_keys + 80]
    aesenc xmm0, xmmword ptr [rip + aes_round_keys + 96]
    aesenc xmm0, xmmword ptr [rip + aes_round_keys + 112]
    aesenc xmm0, xmmword ptr [rip + aes_round_keys + 128]
    aesenc xmm0, xmmword ptr [rip + aes_round_keys + 144]
    aesenclast xmm0, xmmword ptr [rip + aes_round_keys + 160]

    movdqu xmm1, xmmword ptr [rdi]
    pxor xmm0, xmm1
    movdqu xmmword ptr [rdi], xmm0

    add rdi, 16
    sub rsi, 16
    jnz .Laes_loop
    mov qword ptr [rip + g_aes_ctr], r9
.Laes_done:
    ret

# ==============================================================================
# Helper: write_all_fd(edi = fd, rsi = buf, rdx = len) -> eax (0 = ok, -1 = err)
# ==============================================================================
write_all_fd:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    push r13
    sub rsp, 8

    mov ebx, edi
    mov r12, rsi
    mov r13, rdx

.Lw_loop:
    test r13, r13
    jz .Lw_ok
    mov edi, ebx
    mov rsi, r12
    mov rdx, r13
    call write
    cmp rax, 0
    jle .Lw_err
    add r12, rax
    sub r13, rax
    jmp .Lw_loop

.Lw_ok:
    xor eax, eax
    jmp .Lw_ret
.Lw_err:
    mov eax, -1
.Lw_ret:
    add rsp, 8
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: read_all_fd(edi = fd, rsi = buf, rdx = len) -> eax (0 = ok, -1 = err)
# ==============================================================================
read_all_fd:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    push r13
    sub rsp, 8

    mov ebx, edi
    mov r12, rsi
    mov r13, rdx

.Lr_loop:
    test r13, r13
    jz .Lr_ok
    mov edi, ebx
    mov rsi, r12
    mov rdx, r13
    call read
    cmp rax, 0
    jle .Lr_err
    add r12, rax
    sub r13, rax
    jmp .Lr_loop

.Lr_ok:
    xor eax, eax
    jmp .Lr_ret
.Lr_err:
    mov eax, -1
.Lr_ret:
    add rsp, 8
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: out_flush() -> eax (0 = ok, -1 = err)
# Encrypts g_out_buf with AES-128-CTR if g_is_nsp == 1 and writes to g_out_fd
# ==============================================================================
out_flush:
    push rbp
    mov rbp, rsp
    mov edx, dword ptr [rip + g_out_pos]
    test edx, edx
    jz .Lof_ok

    cmp dword ptr [rip + g_is_nsp], 0
    je .Lof_write
    lea rdi, [rip + g_out_buf]
    mov esi, edx
    call aes_ctr_encrypt_buf

.Lof_write:
    mov edi, dword ptr [rip + g_out_fd]
    lea rsi, [rip + g_out_buf]
    mov edx, dword ptr [rip + g_out_pos]
    mov dword ptr [rip + g_out_pos], 0
    call write_all_fd
    pop rbp
    ret

.Lof_ok:
    xor eax, eax
    pop rbp
    ret

# ==============================================================================
# Helper: out_emit(rdi = src_ptr, rsi = len) -> eax (0 = ok, -1 = err)
# ==============================================================================
out_emit:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    push r13
    sub rsp, 8

    mov r12, rdi                  # src_ptr
    mov r13, rsi                  # remaining len

.Loe_loop:
    test r13, r13
    jz .Loe_ok

    mov eax, 65536
    mov edx, dword ptr [rip + g_out_pos]
    sub eax, edx                  # space in g_out_buf
    mov rbx, r13
    cmp rbx, rax
    cmova rbx, rax                # chunk = min(rem, space)

    lea rdi, [rip + g_out_buf]
    add rdi, rdx
    mov rsi, r12
    mov rcx, rbx
    rep movsb

    add dword ptr [rip + g_out_pos], ebx
    add r12, rbx
    sub r13, rbx

    cmp dword ptr [rip + g_out_pos], 65536
    jne .Loe_loop
    call out_flush
    test eax, eax
    jnz .Loe_err
    jmp .Loe_loop

.Loe_ok:
    xor eax, eax
    jmp .Loe_ret
.Loe_err:
    mov eax, -1
.Loe_ret:
    add rsp, 8
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: copy_fd_range(edi = in_fd, esi = out_fd, rdx = byte_count) -> eax
# ==============================================================================
copy_fd_range:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    push r13
    sub rsp, 8

    mov ebx, edi                  # in_fd
    mov r12d, esi                 # out_fd
    mov r13, rdx                  # remaining

.Lcfr_loop:
    test r13, r13
    jz .Lcfr_ok
    mov rdx, 65536
    cmp r13, rdx
    cmovb rdx, r13
    mov edi, ebx
    lea rsi, [rip + g_io_buf]
    call read_all_fd
    test eax, eax
    jnz .Lcfr_err

    mov rdx, 65536
    cmp r13, rdx
    cmovb rdx, r13
    sub r13, rdx
    mov edi, r12d
    lea rsi, [rip + g_io_buf]
    call write_all_fd
    test eax, eax
    jnz .Lcfr_err
    jmp .Lcfr_loop

.Lcfr_ok:
    xor eax, eax
    jmp .Lcfr_ret
.Lcfr_err:
    mov eax, -1
.Lcfr_ret:
    add rsp, 8
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: open_and_validate_iso() -> eax (iso_fd >= 0, or -1 on error)
# Loads FST into g_fst_buf and verifies GameCube magic 0xC2339F3D
# ==============================================================================
open_and_validate_iso:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    sub rsp, 16

    cmp byte ptr [rip + g_iso_path], 0
    jne .Loav_open
    # If running in GUI mode and no ISO selected yet, open file picker
    cmp qword ptr [rip + g_dpy], 0
    je .Loav_no_iso
    call browse_iso_zenity
    cmp byte ptr [rip + g_iso_path], 0
    je .Loav_no_iso

.Loav_open:
    lea rdi, [rip + g_iso_path]
    mov esi, O_RDONLY
    xor edx, edx
    call open
    cmp eax, 0
    jl .Loav_open_err
    mov ebx, eax                  # iso_fd

    # Read 0x440 header
    mov edi, ebx
    lea rsi, [rip + g_io_buf]
    mov edx, 0x440
    call read_all_fd
    test eax, eax
    jnz .Loav_bad_iso

    # Check GameCube disc magic at +0x1C (0xC2339F3D BE == 0x3D9F33C2 LE)
    mov eax, dword ptr [rip + g_io_buf + 0x1C]
    cmp eax, 0x3D9F33C2
    jne .Loav_bad_iso

    # Read fst_off (+0x424 BE) and fst_size (+0x428 BE)
    mov esi, dword ptr [rip + g_io_buf + 0x424]
    bswap esi
    mov r12d, dword ptr [rip + g_io_buf + 0x428]
    bswap r12d
    cmp r12d, 8192
    ja .Loav_bad_iso

    mov edi, ebx
    mov edx, SEEK_SET
    call lseek
    cmp rax, 0
    jl .Loav_bad_iso

    mov edi, ebx
    lea rsi, [rip + g_fst_buf]
    mov edx, r12d
    call read_all_fd
    test eax, eax
    jnz .Loav_bad_iso

    mov dword ptr [rip + g_iso_fd], ebx
    mov eax, ebx
    jmp .Loav_ret

.Loav_no_iso:
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_err_no_iso]
    call strcpy
    mov eax, -1
    jmp .Loav_ret

.Loav_open_err:
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_err_open_iso]
    call strcpy
    mov eax, -1
    jmp .Loav_ret

.Loav_bad_iso:
    mov edi, ebx
    call close
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_err_bad_iso]
    call strcpy
    mov eax, -1

.Loav_ret:
    add rsp, 16
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: stream_romfs_from_iso() -> eax (0 = ok, -1 = err)
# Streams the 751,669,536-byte RomFS image from g_iso_fd into g_out_fd
# ==============================================================================
stream_romfs_from_iso:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    push r13
    push r14
    push r15
    sub rsp, 8

    mov dword ptr [rip + g_out_pos], 0
    mov rax, 0x00147240
    mov qword ptr [rip + g_aes_ctr], rax

    # 1. Emit 560-byte RomFS header + boot.txt + config/fps_counter.txt
    lea rdi, [rip + romfs_prefix_560]
    mov esi, 560
    call out_emit
    test eax, eax
    jnz .Lsr_err

    # 2. Loop through all 186 ISO files in romfs_iso_table
    xor r12d, r12d                # idx = 0 .. 185
    lea r13, [rip + romfs_iso_table]

.Lsr_file_loop:
    cmp r12d, 186
    jge .Lsr_footer

    mov eax, dword ptr [r13]      # fst_idx
    mov r14d, dword ptr [r13 + 4] # file_size
    mov r15d, dword ptr [r13 + 8] # pad_len

    # Lookup file offset in g_fst_buf[fst_idx * 12 + 4]
    imul eax, 12
    lea rcx, [rip + g_fst_buf]
    mov esi, dword ptr [rcx + rax + 4]
    bswap esi                     # iso_offset

    mov edi, dword ptr [rip + g_iso_fd]
    mov edx, SEEK_SET
    call lseek
    cmp rax, 0
    jl .Lsr_err

.Lsr_chunk_loop:
    test r14d, r14d
    jz .Lsr_pad
    mov ebx, 65536
    cmp r14d, ebx
    cmovb ebx, r14d

    mov edi, dword ptr [rip + g_iso_fd]
    lea rsi, [rip + g_io_buf]
    mov edx, ebx
    call read_all_fd
    test eax, eax
    jnz .Lsr_err

    lea rdi, [rip + g_io_buf]
    mov esi, ebx
    call out_emit
    test eax, eax
    jnz .Lsr_err

    sub r14d, ebx
    jmp .Lsr_chunk_loop

.Lsr_pad:
    test r15d, r15d
    jz .Lsr_next_file
    lea rdi, [rip + zero_pad_16]
    mov esi, r15d
    call out_emit
    test eax, eax
    jnz .Lsr_err

.Lsr_next_file:
    # Update GUI progress bar every 8 files if X11 window is active
    mov eax, r12d
    and eax, 7
    jnz .Lsr_step
    mov eax, r12d
    imul eax, 85
    mov ecx, 186
    cdq
    idiv ecx
    add eax, 10
    mov dword ptr [rip + g_progress_pct], eax
    cmp qword ptr [rip + g_dpy], 0
    je .Lsr_step
    call render_gui

.Lsr_step:
    inc r12d
    add r13, 12
    jmp .Lsr_file_loop

.Lsr_footer:
    # 3. Emit 9,348-byte RomFS directory/file metadata footer & flush
    lea rdi, [rip + romfs_footer_9348]
    mov esi, 9348
    call out_emit
    test eax, eax
    jnz .Lsr_err

    call out_flush
    test eax, eax
    jnz .Lsr_err

    xor eax, eax
    jmp .Lsr_ret

.Lsr_err:
    mov eax, -1
.Lsr_ret:
    add rsp, 8
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Native x86_64 NRO Builder: build_nro_native() -> eax (0 = ok, -1 = err)
# Produces target/switch/tssm.nro (782,999,087 bytes)
# ==============================================================================
build_nro_native:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    sub rsp, 16

    call open_and_validate_iso
    cmp eax, 0
    jl .Lbnro_fail

    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_status_building_nro]
    call strcpy
    mov dword ptr [rip + g_progress_pct], 5
    cmp qword ptr [rip + g_dpy], 0
    je .Lbnro_mkdirs
    call render_gui

.Lbnro_mkdirs:
    lea rdi, [rip + str_dir_target]
    mov esi, 0755
    call mkdir
    lea rdi, [rip + str_dir_switch]
    mov esi, 0755
    call mkdir

    # Open tools/switch/tssm_engine.nro (31,329,551 bytes)
    lea rdi, [rip + str_path_engine_nro]
    mov esi, O_RDONLY
    xor edx, edx
    call open
    cmp eax, 0
    jl .Lbnro_no_tpl
    mov ebx, eax                  # tpl_fd

    # Create target/switch/tssm.nro
    lea rdi, [rip + str_path_out_nro]
    mov esi, O_WRONLY_CREAT_TRUNC
    mov edx, 0755
    call open
    cmp eax, 0
    jl .Lbnro_close_tpl
    mov r12d, eax                 # out_fd
    mov dword ptr [rip + g_out_fd], r12d

    # Copy 31,329,551-byte NRO engine + ASET prefix
    mov edi, ebx
    mov esi, r12d
    mov edx, 31329551
    call copy_fd_range
    mov edi, ebx
    push rax
    call close
    pop rax
    test eax, eax
    jnz .Lbnro_write_err

    # Stream 751,669,536-byte RomFS in plaintext (g_is_nsp = 0)
    mov dword ptr [rip + g_is_nsp], 0
    call stream_romfs_from_iso
    mov ebx, eax

    mov edi, r12d
    call close
    mov edi, dword ptr [rip + g_iso_fd]
    call close

    test ebx, ebx
    jnz .Lbnro_write_err

    mov dword ptr [rip + g_progress_pct], 100
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_status_nro_ok]
    call strcpy
    cmp qword ptr [rip + g_dpy], 0
    je .Lbnro_ok
    call render_gui
.Lbnro_ok:
    xor eax, eax
    jmp .Lbnro_ret

.Lbnro_close_tpl:
    mov edi, ebx
    call close
.Lbnro_no_tpl:
    mov edi, dword ptr [rip + g_iso_fd]
    call close
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_err_tpl_nro]
    call strcpy
    mov eax, -1
    jmp .Lbnro_ret

.Lbnro_write_err:
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_err_write]
    call strcpy
.Lbnro_fail:
    mov eax, -1
.Lbnro_ret:
    cmp qword ptr [rip + g_dpy], 0
    je .Lbnro_exit
    push rax
    call render_gui
    pop rax
.Lbnro_exit:
    add rsp, 16
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Native x86_64 NSP Builder: build_nsp_native() -> eax (0 = ok, -1 = err)
# Produces target/switch/nsp/0100780012340000.nsp (773,292,248 bytes)
# ==============================================================================
build_nsp_native:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    sub rsp, 16

    call open_and_validate_iso
    cmp eax, 0
    jl .Lbnsp_fail

    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_status_building_nsp]
    call strcpy
    mov dword ptr [rip + g_progress_pct], 5
    cmp qword ptr [rip + g_dpy], 0
    je .Lbnsp_mkdirs
    call render_gui

.Lbnsp_mkdirs:
    lea rdi, [rip + str_dir_target]
    mov esi, 0755
    call mkdir
    lea rdi, [rip + str_dir_switch]
    mov esi, 0755
    call mkdir
    lea rdi, [rip + str_dir_nsp]
    mov esi, 0755
    call mkdir

    # Open tools/switch/tssm_nsp_template.bin (21,622,712 bytes)
    lea rdi, [rip + str_path_nsp_tpl]
    mov esi, O_RDONLY
    xor edx, edx
    call open
    cmp eax, 0
    jl .Lbnsp_no_tpl
    mov ebx, eax                  # tpl_fd

    # Create target/switch/nsp/0100780012340000.nsp
    lea rdi, [rip + str_path_out_nsp]
    mov esi, O_WRONLY_CREAT_TRUNC
    mov edx, 0644
    call open
    cmp eax, 0
    jl .Lbnsp_close_tpl
    mov r12d, eax                 # out_fd
    mov dword ptr [rip + g_out_fd], r12d

    # 1. Copy 21,439,704-byte NSP prefix (PFS0 + program.nca ExeFS + IVFC L1..L5)
    mov edi, ebx
    mov esi, r12d
    mov edx, 21439704
    call copy_fd_range
    test eax, eax
    jnz .Lbnsp_err_close

    # 2. Stream & AES-128-CTR encrypt 751,669,536-byte RomFS Level 6 (g_is_nsp = 1)
    mov dword ptr [rip + g_is_nsp], 1
    call stream_romfs_from_iso
    test eax, eax
    jnz .Lbnsp_err_close

    # 3. Copy 183,008-byte NSP suffix (NCA tail + control.nca + meta.cnmt.nca)
    mov edi, ebx
    mov esi, r12d
    mov edx, 183008
    call copy_fd_range
    test eax, eax
    jnz .Lbnsp_err_close

    mov edi, ebx
    call close
    mov edi, r12d
    call close
    mov edi, dword ptr [rip + g_iso_fd]
    call close

    mov dword ptr [rip + g_progress_pct], 100
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_status_nsp_ok]
    call strcpy
    cmp qword ptr [rip + g_dpy], 0
    je .Lbnsp_ok
    call render_gui
.Lbnsp_ok:
    xor eax, eax
    jmp .Lbnsp_ret

.Lbnsp_err_close:
    mov edi, ebx
    call close
    mov edi, r12d
    call close
    mov edi, dword ptr [rip + g_iso_fd]
    call close
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_err_write]
    call strcpy
    mov eax, -1
    jmp .Lbnsp_ret

.Lbnsp_close_tpl:
    mov edi, ebx
    call close
.Lbnsp_no_tpl:
    mov edi, dword ptr [rip + g_iso_fd]
    call close
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_err_tpl_nsp]
    call strcpy
.Lbnsp_fail:
    mov eax, -1
.Lbnsp_ret:
    cmp qword ptr [rip + g_dpy], 0
    je .Lbnsp_exit
    push rax
    call render_gui
    pop rax
.Lbnsp_exit:
    add rsp, 16
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: fill_rect(edi = x, esi = y, edx = w, ecx = h, r8d = color)
# ==============================================================================
fill_rect:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    push r13
    push r14

    mov ebx, edi                  # x
    mov r12d, esi                 # y
    mov r13d, edx                 # w
    mov r14d, ecx                 # h

    mov rdi, qword ptr [rip + g_dpy]
    mov rsi, qword ptr [rip + g_gc]
    mov edx, r8d
    call XSetForeground

    mov rdi, qword ptr [rip + g_dpy]
    mov rsi, qword ptr [rip + g_win]
    mov rdx, qword ptr [rip + g_gc]
    mov ecx, ebx                  # x
    mov r8d, r12d                 # y
    mov r9d, r13d                 # width
    sub rsp, 16
    mov dword ptr [rsp], r14d     # height (7th arg on stack)
    call XFillRectangle
    add rsp, 16

    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: draw_str(edi = x, esi = y, rdx = str_ptr, ecx = color)
# ==============================================================================
draw_str:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    push r13
    sub rsp, 8

    mov ebx, edi                  # x
    mov r12d, esi                 # y
    mov r13, rdx                  # str_ptr

    mov rdi, qword ptr [rip + g_dpy]
    mov rsi, qword ptr [rip + g_gc]
    mov edx, ecx                  # color
    call XSetForeground

    mov rdi, r13
    call strlen
    mov ecx, eax                  # len

    mov rdi, qword ptr [rip + g_dpy]
    mov rsi, qword ptr [rip + g_win]
    mov rdx, qword ptr [rip + g_gc]
    sub rsp, 16
    mov dword ptr [rsp], ecx      # length (7th arg on stack)
    mov r9, r13                   # string (6th arg)
    mov r8d, r12d                 # y (5th arg)
    mov ecx, ebx                  # x (4th arg)
    call XDrawString
    add rsp, 16

    add rsp, 8
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: draw_btn(edi = id, esi = x, edx = y, ecx = w, r8d = h, r9d = bg_col,
#                  [rbp+16] = hover_col, [rbp+24] = txt_col, [rbp+32] = label,
#                  [rbp+40] = txt_off_x)
# ==============================================================================
draw_btn:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    push r13
    push r14
    push r15
    sub rsp, 8

    mov ebx, edi                  # id
    mov r12d, esi                 # x
    mov r13d, edx                 # y
    mov r14d, ecx                 # w
    mov r15d, r8d                 # h

    mov eax, r9d                  # bg_col
    cmp ebx, dword ptr [rip + g_hover_btn]
    jne .Ldb_fill
    mov eax, dword ptr [rbp + 16] # hover_col
.Ldb_fill:
    mov r8d, eax
    mov ecx, r15d
    mov edx, r14d
    mov esi, r13d
    mov edi, r12d
    call fill_rect

    # Hover highlight bar at top of button
    cmp ebx, dword ptr [rip + g_hover_btn]
    jne .Ldb_text
    mov r8d, COL_TEXT_WHITE
    mov ecx, 2
    mov edx, r14d
    mov esi, r13d
    mov edi, r12d
    call fill_rect

.Ldb_text:
    mov eax, r15d
    shr eax, 1
    lea esi, [r13 + rax + 5]      # baseline y
    mov edi, r12d
    add edi, dword ptr [rbp + 40] # x + txt_off_x
    mov rdx, qword ptr [rbp + 32] # label
    mov ecx, dword ptr [rbp + 24] # txt_col
    call draw_str

    add rsp, 8
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: hit_test(edi = mx, esi = my) -> eax = btn_id (0..7)
# ==============================================================================
hit_test:
    # Button 1: Browse ISO (590, 118, 128, 34) -> x:590..718, y:118..152
    cmp edi, 590
    jl .Lht_chk2
    cmp edi, 718
    jge .Lht_chk2
    cmp esi, 118
    jl .Lht_chk2
    cmp esi, 152
    jge .Lht_chk2
    mov eax, 1
    ret

.Lht_chk2:
    # Toggle 2: Release Profile (44, 212, 316, 30) -> x:44..360, y:212..242
    cmp edi, 44
    jl .Lht_chk3
    cmp edi, 360
    jge .Lht_chk3
    cmp esi, 212
    jl .Lht_chk3
    cmp esi, 242
    jge .Lht_chk3
    mov eax, 2
    ret

.Lht_chk3:
    # Toggle 3: NSP Packaging (394, 212, 322, 30) -> x:394..716, y:212..242
    cmp edi, 394
    jl .Lht_chk4
    cmp edi, 716
    jge .Lht_chk4
    cmp esi, 212
    jl .Lht_chk4
    cmp esi, 242
    jge .Lht_chk4
    mov eax, 3
    ret

.Lht_chk4:
    # Button 4: Build Switch (.nro + .nsp) (44, 304, 322, 52) -> x:44..366, y:304..356
    cmp edi, 44
    jl .Lht_chk5
    cmp edi, 366
    jge .Lht_chk5
    cmp esi, 304
    jl .Lht_chk5
    cmp esi, 356
    jge .Lht_chk5
    mov eax, 4
    ret

.Lht_chk5:
    # Button 5: Build Switch NSP Only (394, 304, 322, 52) -> x:394..716, y:304..356
    cmp edi, 394
    jl .Lht_chk6
    cmp edi, 716
    jge .Lht_chk6
    cmp esi, 304
    jl .Lht_chk6
    cmp esi, 356
    jge .Lht_chk6
    mov eax, 5
    ret

.Lht_chk6:
    # Button 6: Build Switch NRO Only (44, 370, 322, 46) -> x:44..366, y:370..416
    cmp edi, 44
    jl .Lht_chk7
    cmp edi, 366
    jge .Lht_chk7
    cmp esi, 370
    jl .Lht_chk7
    cmp esi, 416
    jge .Lht_chk7
    mov eax, 6
    ret

.Lht_chk7:
    # Button 7: Open Output Folder (394, 370, 322, 46) -> x:394..716, y:370..416
    cmp edi, 394
    jl .Lht_none
    cmp edi, 716
    jge .Lht_none
    cmp esi, 370
    jl .Lht_none
    cmp esi, 416
    jge .Lht_none
    mov eax, 7
    ret

.Lht_none:
    xor eax, eax
    ret

# ==============================================================================
# Helper: spawn_shell_cmd(rdi = cmd_str)
# ==============================================================================
spawn_shell_cmd:
    push rbp
    mov rbp, rsp
    push rbx
    sub rsp, 8
    mov rbx, rdi

    call fork
    test eax, eax
    jnz .Lspawn_parent

    lea rdi, [rip + str_bin_sh]
    lea rsi, [rip + str_sh_arg0]
    lea rdx, [rip + str_sh_arg1]
    mov rcx, rbx
    xor r8d, r8d
    call execl
    xor edi, edi
    call _exit

.Lspawn_parent:
    add rsp, 8
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: auto_detect_iso()
# Detects any *.iso or *.gcm file in the current directory on startup
# ==============================================================================
auto_detect_iso:
    push rbp
    mov rbp, rsp
    push rbx
    sub rsp, 8

    lea rdi, [rip + str_default_iso]
    xor esi, esi
    call access
    test eax, eax
    jnz .Lad_glob
    lea rdi, [rip + g_iso_path]
    lea rsi, [rip + str_default_iso]
    call strcpy
    mov dword ptr [rip + g_progress_pct], 25
    jmp .Lad_done

.Lad_glob:
    lea rdi, [rip + str_find_iso_cmd]
    lea rsi, [rip + str_popen_r]
    call popen
    test rax, rax
    jz .Lad_done
    mov rbx, rax

    lea rdi, [rip + g_iso_path]
    mov esi, 500
    mov rdx, rbx
    call fgets
    test rax, rax
    jz .Lad_close

    lea rdi, [rip + g_iso_path]
    call strlen
    test eax, eax
    jz .Lad_close
    lea rcx, [rip + g_iso_path]
    cmp byte ptr [rcx + rax - 1], 10
    jne .Lad_found
    mov byte ptr [rcx + rax - 1], 0
.Lad_found:
    mov dword ptr [rip + g_progress_pct], 25
.Lad_close:
    mov rdi, rbx
    call pclose
.Lad_done:
    add rsp, 8
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: browse_iso_zenity()
# ==============================================================================
browse_iso_zenity:
    push rbp
    mov rbp, rsp
    push rbx
    sub rsp, 8

    lea rdi, [rip + str_zenity_cmd]
    lea rsi, [rip + str_popen_r]
    call popen
    test rax, rax
    jz .Lzen_done
    mov rbx, rax

    lea rdi, [rip + g_iso_path]
    mov esi, 500
    mov rdx, rbx
    call fgets
    test rax, rax
    jz .Lzen_close

    lea rdi, [rip + g_iso_path]
    call strlen
    test eax, eax
    jz .Lzen_close
    lea rcx, [rip + g_iso_path]
    cmp byte ptr [rcx + rax - 1], 10
    jne .Lzen_status
    mov byte ptr [rcx + rax - 1], 0

.Lzen_status:
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_status_iso_set]
    call strcpy
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + g_iso_path]
    call strcat
    mov dword ptr [rip + g_progress_pct], 30

.Lzen_close:
    mov rdi, rbx
    call pclose
.Lzen_done:
    add rsp, 8
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Render Entire GUI Window: render_gui()
# ==============================================================================
render_gui:
    push rbp
    mov rbp, rsp

    # 1. Window background (0, 0, 760, 520)
    mov r8d, COL_BG_DARK
    mov ecx, 520
    mov edx, 760
    xor esi, esi
    xor edi, edi
    call fill_rect

    # 2. Header Banner (0, 0, 760, 74) + SpongeBob Gold accent bar (0, 74, 760, 3)
    mov r8d, COL_HEADER_BG
    mov ecx, 74
    mov edx, 760
    xor esi, esi
    xor edi, edi
    call fill_rect

    mov r8d, COL_GOLD
    mov ecx, 3
    mov edx, 760
    mov esi, 74
    xor edi, edi
    call fill_rect

    mov ecx, COL_GOLD
    lea rdx, [rip + str_header_title]
    mov esi, 30
    mov edi, 28
    call draw_str

    mov ecx, COL_TEXT_MUTED
    lea rdx, [rip + str_header_sub]
    mov esi, 54
    mov edi, 28
    call draw_str

    # 3. Card 1: GameCube Disc Image (24, 92, 712, 76)
    mov r8d, COL_CARD_BORDER
    mov ecx, 78
    mov edx, 714
    mov esi, 91
    mov edi, 23
    call fill_rect

    mov r8d, COL_CARD_BG
    mov ecx, 76
    mov edx, 712
    mov esi, 92
    mov edi, 24
    call fill_rect

    mov ecx, COL_TEXT_WHITE
    lea rdx, [rip + str_card1_title]
    mov esi, 110
    mov edi, 40
    call draw_str

    # ISO Input Box (40, 120, 538, 30)
    mov r8d, COL_INPUT_BG
    mov ecx, 30
    mov edx, 538
    mov esi, 120
    mov edi, 40
    call fill_rect

    lea rdx, [rip + g_iso_path]
    mov ecx, COL_GREEN
    cmp byte ptr [rdx], 0
    jne .Lrg_iso_draw
    lea rdx, [rip + str_iso_placeholder]
    mov ecx, COL_TEXT_MUTED
.Lrg_iso_draw:
    mov esi, 139
    mov edi, 50
    call draw_str

    # Button 1: Browse ISO (590, 118, 128, 34)
    sub rsp, 48
    mov dword ptr [rsp + 24], 20
    lea rax, [rip + str_btn_browse]
    mov qword ptr [rsp + 16], rax
    mov dword ptr [rsp + 8], COL_TEXT_WHITE
    mov dword ptr [rsp], COL_SLATE_HOVER
    mov r9d, COL_SLATE
    mov r8d, 34
    mov ecx, 128
    mov edx, 118
    mov esi, 590
    mov edi, 1
    call draw_btn
    add rsp, 48

    # 4. Card 2: Compiler & Packaging Profile (24, 180, 712, 74)
    mov r8d, COL_CARD_BORDER
    mov ecx, 76
    mov edx, 714
    mov esi, 179
    mov edi, 23
    call fill_rect

    mov r8d, COL_CARD_BG
    mov ecx, 74
    mov edx, 712
    mov esi, 180
    mov edi, 24
    call fill_rect

    mov ecx, COL_TEXT_WHITE
    lea rdx, [rip + str_card2_title]
    mov esi, 198
    mov edi, 40
    call draw_str

    # Toggle 2: Release Profile (44, 212, 316, 30)
    sub rsp, 48
    mov dword ptr [rsp + 24], 14
    lea rax, [rip + str_opt_rel_on]
    mov r9d, COL_TEAL
    cmp dword ptr [rip + g_opt_release], 0
    jne .Lrg_t2
    lea rax, [rip + str_opt_rel_off]
    mov r9d, COL_SLATE
.Lrg_t2:
    mov qword ptr [rsp + 16], rax
    mov dword ptr [rsp + 8], COL_TEXT_WHITE
    mov dword ptr [rsp], COL_TEAL_HOVER
    mov r8d, 30
    mov ecx, 316
    mov edx, 212
    mov esi, 44
    mov edi, 2
    call draw_btn
    add rsp, 48

    # Toggle 3: Package Standalone NSP (394, 212, 322, 30)
    sub rsp, 48
    mov dword ptr [rsp + 24], 14
    lea rax, [rip + str_opt_nsp_on]
    mov r9d, COL_TEAL
    cmp dword ptr [rip + g_opt_nsp], 0
    jne .Lrg_t3
    lea rax, [rip + str_opt_nsp_off]
    mov r9d, COL_SLATE
.Lrg_t3:
    mov qword ptr [rsp + 16], rax
    mov dword ptr [rsp + 8], COL_TEXT_WHITE
    mov dword ptr [rsp], COL_TEAL_HOVER
    mov r8d, 30
    mov ecx, 322
    mov edx, 212
    mov esi, 394
    mov edi, 3
    call draw_btn
    add rsp, 48

    # 5. Card 3: Build & Package Targets (24, 266, 712, 166)
    mov r8d, COL_CARD_BORDER
    mov ecx, 168
    mov edx, 714
    mov esi, 265
    mov edi, 23
    call fill_rect

    mov r8d, COL_CARD_BG
    mov ecx, 166
    mov edx, 712
    mov esi, 266
    mov edi, 24
    call fill_rect

    mov ecx, COL_TEXT_WHITE
    lea rdx, [rip + str_card3_title]
    mov esi, 286
    mov edi, 40
    call draw_str

    # Button 4: BUILD NINTENDO SWITCH (.NRO + .NSP) (44, 304, 322, 52)
    sub rsp, 48
    mov dword ptr [rsp + 24], 24
    lea rax, [rip + str_btn_build_sw]
    mov qword ptr [rsp + 16], rax
    mov dword ptr [rsp + 8], COL_TEXT_DARK
    mov dword ptr [rsp], COL_GOLD_HOVER
    mov r9d, COL_GOLD
    mov r8d, 52
    mov ecx, 322
    mov edx, 304
    mov esi, 44
    mov edi, 4
    call draw_btn
    add rsp, 48

    # Button 5: BUILD SWITCH .NSP ONLY (394, 304, 322, 52)
    sub rsp, 48
    mov dword ptr [rsp + 24], 36
    lea rax, [rip + str_btn_pkg_nsp]
    mov qword ptr [rsp + 16], rax
    mov dword ptr [rsp + 8], COL_TEXT_DARK
    mov dword ptr [rsp], COL_CYAN_HOVER
    mov r9d, COL_CYAN
    mov r8d, 52
    mov ecx, 322
    mov edx, 304
    mov esi, 394
    mov edi, 5
    call draw_btn
    add rsp, 48

    # Button 6: BUILD SWITCH .NRO ONLY (44, 370, 322, 46)
    sub rsp, 48
    mov dword ptr [rsp + 24], 38
    lea rax, [rip + str_btn_extract]
    mov qword ptr [rsp + 16], rax
    mov dword ptr [rsp + 8], COL_TEXT_WHITE
    mov dword ptr [rsp], COL_TEAL_HOVER
    mov r9d, COL_TEAL
    mov r8d, 46
    mov ecx, 322
    mov edx, 370
    mov esi, 44
    mov edi, 6
    call draw_btn
    add rsp, 48

    # Button 7: OPEN OUTPUT FOLDER (394, 370, 322, 46)
    sub rsp, 48
    mov dword ptr [rsp + 24], 38
    lea rax, [rip + str_btn_open_dir]
    mov qword ptr [rsp + 16], rax
    mov dword ptr [rsp + 8], COL_TEXT_WHITE
    mov dword ptr [rsp], COL_SLATE_HOVER
    mov r9d, COL_SLATE
    mov r8d, 46
    mov ecx, 322
    mov edx, 370
    mov esi, 394
    mov edi, 7
    call draw_btn
    add rsp, 48

    # 6. Status & Progress Footer (24, 444, 712, 52)
    mov r8d, COL_INPUT_BG
    mov ecx, 52
    mov edx, 712
    mov esi, 444
    mov edi, 24
    call fill_rect

    # Progress Track (36, 478, 688, 10)
    mov r8d, COL_CARD_BG
    mov ecx, 10
    mov edx, 688
    mov esi, 478
    mov edi, 36
    call fill_rect

    # Progress Fill
    mov eax, dword ptr [rip + g_progress_pct]
    imul eax, 688
    mov ecx, 100
    cdq
    idiv ecx
    mov r8d, COL_GREEN
    mov ecx, 10
    mov edx, eax
    mov esi, 478
    mov edi, 36
    call fill_rect

    mov ecx, COL_TEXT_WHITE
    lea rdx, [rip + g_status_text]
    mov esi, 466
    mov edi, 38
    call draw_str

    mov rdi, qword ptr [rip + g_dpy]
    call XFlush

    pop rbp
    ret

# ==============================================================================
# Entry Point: main(edi = argc, rsi = argv)
# ==============================================================================
.globl main
main:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    push r13
    sub rsp, 216                  # 192 bytes for XEvent + locals

    mov r12d, edi                 # argc
    mov r13, rsi                  # argv

    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_status_ready]
    call strcpy

    call auto_detect_iso

    # Check CLI arguments: --check, --help, --build-nro, --build-nsp, --build-all
    cmp r12d, 1
    jle .Lopen_x11
    mov rbx, qword ptr [r13 + 8]

    # If a second argument (ISO path) was passed, copy it to g_iso_path
    cmp r12d, 2
    jle .Lchk_flags
    lea rdi, [rip + g_iso_path]
    mov rsi, qword ptr [r13 + 16]
    call strcpy

.Lchk_flags:
    mov rdi, rbx
    lea rsi, [rip + str_arg_check]
    call strcmp
    test eax, eax
    jz .Lcli_check
    mov rdi, rbx
    lea rsi, [rip + str_arg_help]
    call strcmp
    test eax, eax
    jz .Lcli_check

    mov rdi, rbx
    lea rsi, [rip + str_arg_nro]
    call strcmp
    test eax, eax
    jz .Lcli_build_nro

    mov rdi, rbx
    lea rsi, [rip + str_arg_nsp]
    call strcmp
    test eax, eax
    jz .Lcli_build_nsp

    mov rdi, rbx
    lea rsi, [rip + str_arg_all]
    call strcmp
    test eax, eax
    jz .Lcli_build_all

.Lopen_x11:
    xor edi, edi
    call XOpenDisplay
    test rax, rax
    jz .Lno_display
    mov qword ptr [rip + g_dpy], rax

    mov rdi, rax
    call XDefaultScreen
    mov dword ptr [rip + g_screen], eax

    mov rdi, qword ptr [rip + g_dpy]
    mov esi, eax
    call XRootWindow
    mov rbx, rax

    mov rdi, qword ptr [rip + g_dpy]
    mov esi, dword ptr [rip + g_screen]
    call XDefaultGC
    mov qword ptr [rip + g_gc], rax

    # XCreateSimpleWindow(dpy, root, 120, 120, 760, 520, 0, COL_BG_DARK, COL_BG_DARK)
    mov rdi, qword ptr [rip + g_dpy]
    mov rsi, rbx
    mov edx, 120
    mov ecx, 120
    mov r8d, 760
    mov r9d, 520
    sub rsp, 32
    mov qword ptr [rsp + 16], COL_BG_DARK
    mov qword ptr [rsp + 8], COL_BG_DARK
    mov qword ptr [rsp], 0
    call XCreateSimpleWindow
    add rsp, 32
    mov qword ptr [rip + g_win], rax

    mov rdi, qword ptr [rip + g_dpy]
    mov rsi, rax
    lea rdx, [rip + str_win_title]
    call XStoreName

    mov rdi, qword ptr [rip + g_dpy]
    lea rsi, [rip + str_wm_delete]
    xor edx, edx
    call XInternAtom
    mov qword ptr [rip + g_wm_delete], rax

    mov rdi, qword ptr [rip + g_dpy]
    mov rsi, qword ptr [rip + g_win]
    lea rdx, [rip + g_wm_delete]
    mov ecx, 1
    call XSetWMProtocols

    mov rdi, qword ptr [rip + g_dpy]
    mov rsi, qword ptr [rip + g_win]
    mov edx, EVENT_MASK
    call XSelectInput

    mov rdi, qword ptr [rip + g_dpy]
    mov rsi, qword ptr [rip + g_win]
    call XMapWindow

    call render_gui

.Levent_loop:
    mov rdi, qword ptr [rip + g_dpy]
    lea rsi, [rbp - 216]
    call XNextEvent

    mov eax, dword ptr [rbp - 216]
    cmp eax, Expose
    je .Lev_expose
    cmp eax, MotionNotify
    je .Lev_motion
    cmp eax, ButtonPress
    je .Lev_button
    cmp eax, ClientMessage
    je .Lclean_exit
    jmp .Levent_loop

.Lev_expose:
    call render_gui
    jmp .Levent_loop

.Lev_motion:
    mov edi, dword ptr [rbp - 216 + 64]
    mov esi, dword ptr [rbp - 216 + 68]
    call hit_test
    cmp eax, dword ptr [rip + g_hover_btn]
    je .Levent_loop
    mov dword ptr [rip + g_hover_btn], eax
    call render_gui
    jmp .Levent_loop

.Lev_button:
    mov edi, dword ptr [rbp - 216 + 64]
    mov esi, dword ptr [rbp - 216 + 68]
    call hit_test
    cmp eax, 1
    je .Lact_browse
    cmp eax, 2
    je .Lact_toggle_rel
    cmp eax, 3
    je .Lact_toggle_nsp
    cmp eax, 4
    je .Lact_build_both
    cmp eax, 5
    je .Lact_build_nsp
    cmp eax, 6
    je .Lact_build_nro
    cmp eax, 7
    je .Lact_open_dir
    jmp .Levent_loop

.Lact_browse:
    call browse_iso_zenity
    call render_gui
    jmp .Levent_loop

.Lact_toggle_rel:
    xor dword ptr [rip + g_opt_release], 1
    call render_gui
    jmp .Levent_loop

.Lact_toggle_nsp:
    xor dword ptr [rip + g_opt_nsp], 1
    call render_gui
    jmp .Levent_loop

.Lact_build_both:
    call build_nro_native
    test eax, eax
    jnz .Levent_loop
    cmp dword ptr [rip + g_opt_nsp], 0
    je .Levent_loop
    call build_nsp_native
    test eax, eax
    jnz .Levent_loop
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_status_both_ok]
    call strcpy
    call render_gui
    jmp .Levent_loop

.Lact_build_nsp:
    call build_nsp_native
    jmp .Levent_loop

.Lact_build_nro:
    call build_nro_native
    jmp .Levent_loop

.Lact_open_dir:
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_status_open_dir]
    call strcpy
    call render_gui
    lea rdi, [rip + str_cmd_open_dir]
    call spawn_shell_cmd
    jmp .Levent_loop

.Lclean_exit:
    mov rdi, qword ptr [rip + g_dpy]
    call XCloseDisplay
    xor eax, eax
    jmp .Lmain_ret

.Lcli_build_nro:
    call build_nro_native
    mov ebx, eax
    lea rdi, [rip + g_status_text]
    call puts
    mov eax, ebx
    jmp .Lmain_ret

.Lcli_build_nsp:
    call build_nsp_native
    mov ebx, eax
    lea rdi, [rip + g_status_text]
    call puts
    mov eax, ebx
    jmp .Lmain_ret

.Lcli_build_all:
    call build_nro_native
    test eax, eax
    jnz .Lcli_all_out
    call build_nsp_native
    test eax, eax
    jnz .Lcli_all_out
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_status_both_ok]
    call strcpy
    xor eax, eax
.Lcli_all_out:
    mov ebx, eax
    lea rdi, [rip + g_status_text]
    call puts
    mov eax, ebx
    jmp .Lmain_ret

.Lcli_check:
    lea rdi, [rip + str_cli_banner]
    call puts
    xor eax, eax
    jmp .Lmain_ret

.Lno_display:
    lea rdi, [rip + str_err_display]
    call puts
    mov eax, 1

.Lmain_ret:
    add rsp, 216
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Data Section
# ==============================================================================
.data
    .align 16
zero_pad_16:    .space 16, 0

.include "tools/gui_builder/romfs_tables.inc"

    .align 8
g_dpy:          .quad 0
g_win:          .quad 0
g_gc:           .quad 0
g_wm_delete:    .quad 0
g_aes_ctr:      .quad 0
g_screen:       .long 0
g_hover_btn:    .long 0
g_opt_release:  .long 1
g_opt_nsp:      .long 1
g_progress_pct: .long 15
g_iso_fd:       .long -1
g_out_fd:       .long -1
g_out_pos:      .long 0
g_is_nsp:       .long 0

str_arg_check:
    .asciz "--check"
str_arg_help:
    .asciz "--help"
str_arg_nro:
    .asciz "--build-nro"
str_arg_nsp:
    .asciz "--build-nsp"
str_arg_all:
    .asciz "--build-all"
str_cli_banner:
    .asciz "TSSM Native Port Builder (x86_64 Assembly GUI + CLI) - Options: --build-nro [iso], --build-nsp [iso], --build-all [iso]"
str_err_display:
    .asciz "Error: Cannot open X11 display ($DISPLAY). Use ./tssm-builder --build-all <game.iso> for headless builds."
str_wm_delete:
    .asciz "WM_DELETE_WINDOW"
str_win_title:
    .asciz "The SpongeBob SquarePants Movie - Native Port Builder (x86_64 Assembly GUI)"

str_header_title:
    .asciz "THE SPONGEBOB SQUAREPANTS MOVIE  -  NATIVE PORT BUILDER"
str_header_sub:
    .asciz "Self-Contained x86_64 Assembly + AES-NI Builder  |  GameCube ISO -> Switch .NRO & .NSP"
str_card1_title:
    .asciz "1. GAMECUBE DISC IMAGE (.ISO / .GCM)"
str_card2_title:
    .asciz "2. COMPILER & SWITCH PACKAGING PROFILE"
str_card3_title:
    .asciz "3. BUILD & PACKAGE TARGETS (NATIVE x86_64 ROMFS + AES-NI ENGINE)"
str_iso_placeholder:
    .asciz "No ISO selected (click Browse ISO... or place GameCube ISO in project root)"
str_default_iso:
    .asciz "Nickelodeon SpongeBob SquarePants - The Movie (United Kingdom).iso"
str_btn_browse:
    .asciz "Browse ISO..."
str_opt_rel_on:
    .asciz "[X] Release Profile (Cortex-A57 60 FPS LTO)"
str_opt_rel_off:
    .asciz "[ ] Dev Profile (Fast Debug Build)"
str_opt_nsp_on:
    .asciz "[X] Include NSP in One-Click Build (0100780012340000)"
str_opt_nsp_off:
    .asciz "[ ] Homebrew NRO Only in One-Click Build"
str_btn_build_sw:
    .asciz "BUILD NINTENDO SWITCH (.NRO + .NSP)"
str_btn_pkg_nsp:
    .asciz "BUILD SWITCH .NSP ONLY (738 MB)"
str_btn_extract:
    .asciz "BUILD SWITCH .NRO ONLY (747 MB)"
str_btn_open_dir:
    .asciz "OPEN OUTPUT FOLDER (target/switch)"

str_status_ready:
    .asciz "STATUS: Ready  |  Select your GameCube ISO and click any Build button (no external tools required)."
str_status_iso_set:
    .asciz "STATUS: Disc image selected -> "
str_status_building_nro:
    .asciz "STATUS: Building standalone Homebrew NRO (target/switch/tssm.nro) in x86_64 assembly..."
str_status_building_nsp:
    .asciz "STATUS: Building installable Switch NSP (target/switch/nsp/0100780012340000.nsp) via AES-NI..."
str_status_nro_ok:
    .asciz "SUCCESS: Built target/switch/tssm.nro (747 MB All-in-One Homebrew NRO Ready!)"
str_status_nsp_ok:
    .asciz "SUCCESS: Built target/switch/nsp/0100780012340000.nsp (738 MB Home Menu NSP Ready!)"
str_status_both_ok:
    .asciz "SUCCESS: Built BOTH target/switch/tssm.nro AND target/switch/nsp/0100780012340000.nsp!"
str_status_open_dir:
    .asciz "STATUS: Opened target/switch output directory."

str_err_no_iso:
    .asciz "ERROR: No GameCube ISO selected! Click 'Browse ISO...' to select your .iso/.gcm file."
str_err_open_iso:
    .asciz "ERROR: Could not open the selected GameCube ISO file!"
str_err_bad_iso:
    .asciz "ERROR: Invalid disc image! Please select a valid GameCube ISO (.iso / .gcm)."
str_err_tpl_nro:
    .asciz "ERROR: Missing tools/switch/tssm_engine.nro template in repository!"
str_err_tpl_nsp:
    .asciz "ERROR: Missing tools/switch/tssm_nsp_template.bin template in repository!"
str_err_write:
    .asciz "ERROR: Disk write failed while generating output package!"

str_dir_target:
    .asciz "target"
str_dir_switch:
    .asciz "target/switch"
str_dir_nsp:
    .asciz "target/switch/nsp"
str_path_engine_nro:
    .asciz "tools/switch/tssm_engine.nro"
str_path_nsp_tpl:
    .asciz "tools/switch/tssm_nsp_template.bin"
str_path_out_nro:
    .asciz "target/switch/tssm.nro"
str_path_out_nsp:
    .asciz "target/switch/nsp/0100780012340000.nsp"

str_popen_r:
    .asciz "r"
str_find_iso_cmd:
    .asciz "ls -1 *.iso *.gcm 2>/dev/null | head -n 1"
str_zenity_cmd:
    .asciz "zenity --file-selection --title='Select GameCube ISO' --file-filter='*.iso *.gcm' 2>/dev/null"
str_bin_sh:
    .asciz "/bin/sh"
str_sh_arg0:
    .asciz "sh"
str_sh_arg1:
    .asciz "-c"
str_cmd_open_dir:
    .asciz "mkdir -p target/switch && xdg-open target/switch >/dev/null 2>&1"

.bss
    .align 16
g_iso_path:     .space 520
g_status_text:  .space 600
g_fst_buf:      .space 8192
g_io_buf:       .space 65536
g_out_buf:      .space 65536
