# ==============================================================================
# The SpongeBob SquarePants Movie - Native GUI Builder (Windows x86_64 Assembly)
# ==============================================================================
# Pure x86_64 assembly (GAS Intel syntax) Win32 GDI application that natively
# constructs:
#   - target\switch\tssm.nro (747 MB All-in-One Homebrew NRO with RomFS)
#   - target\switch\nsp\0100780012340000.nsp (738 MB Installable NSP via AES-NI)
#   - Windows PC Port (.exe) via tools\build.ps1
# directly from the user's GameCube ISO with zero external Switch dependencies.
# ==============================================================================

.intel_syntax noprefix
.text

# Win32 Constants
.equ CS_HREDRAW,              0x0002
.equ CS_VREDRAW,              0x0001
.equ IDC_ARROW,               32512
.equ WS_OVERLAPPED,           0x00000000
.equ WS_CAPTION,              0x00C00000
.equ WS_SYSMENU,              0x00080000
.equ WS_MINIMIZEBOX,          0x00020000
.equ WS_VISIBLE,              0x10000000
.equ WS_STYLE_FIXED,          (WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE)
.equ CW_USEDEFAULT,           0x80000000

.equ WM_CREATE,               0x0001
.equ WM_DESTROY,              0x0002
.equ WM_PAINT,                0x000F
.equ WM_CLOSE,                0x0010
.equ WM_ERASEBKGND,           0x0014
.equ WM_MOUSEMOVE,            0x0200
.equ WM_LBUTTONDOWN,          0x0201

.equ TRANSPARENT,             1
.equ FW_NORMAL,               400
.equ FW_SEMIBOLD,             600
.equ FW_BOLD,                 700
.equ DEFAULT_CHARSET,         1
.equ CLEARTYPE_QUALITY,       5
.equ SW_SHOWNORMAL,           1
.equ OFN_FILEMUSTEXIST,       0x00001000
.equ OFN_PATHMUSTEXIST,       0x00000800
.equ OFN_NOCHANGEDIR,         0x00000008
.equ INVALID_FILE_ATTR,       0xFFFFFFFF

.equ GENERIC_READ,            0x80000000
.equ GENERIC_WRITE,           0x40000000
.equ FILE_SHARE_READ,         0x00000001
.equ CREATE_ALWAYS,           2
.equ OPEN_EXISTING,           3
.equ FILE_ATTRIBUTE_NORMAL,   0x00000080
.equ FILE_BEGIN,              0

# Color palette (Win32 COLORREF: 0x00BBGGRR)
.equ COL_BG_DARK,             0x0022130C   # Deep Bikini Bottom Navy (#0C1322)
.equ COL_CARD_BG,             0x00362015   # Card Navy (#152036)
.equ COL_CARD_BORDER,         0x005A3824   # Subtle Card Border (#24385A)
.equ COL_HEADER_BG,           0x002D1A10   # Header Navy (#101A2D)
.equ COL_GOLD,                0x0015C8FF   # SpongeBob Gold (#FFC815)
.equ COL_GOLD_HOVER,          0x0045D8FF   # Bright Gold Hover (#FFD845)
.equ COL_CYAN,                0x00F0B800   # Switch Neon Cyan (#00B8F0)
.equ COL_CYAN_HOVER,          0x00FFCE28   # Bright Cyan Hover (#28CEFF)
.equ COL_TEAL,                0x0096B814   # Seafoam Teal (#14B896)
.equ COL_TEAL_HOVER,          0x00B0D428   # Bright Teal Hover (#28D4B0)
.equ COL_SLATE,               0x00523828   # Slate Button (#283852)
.equ COL_SLATE_HOVER,         0x006E4C36   # Slate Hover (#364C6E)
.equ COL_INPUT_BG,            0x001B0F09   # Dark Input Field (#090F1B)
.equ COL_TEXT_WHITE,          0x00FAFAFA   # Crisp White
.equ COL_TEXT_MUTED,          0x00C8A890   # Soft Ocean Gray-Blue (#90A8C8)
.equ COL_TEXT_DARK,           0x00141008   # Dark text on Gold button
.equ COL_GREEN,               0x0070E030   # Status Ready Green

# External Win32 API imports
.extern GetModuleHandleA
.extern LoadCursorA
.extern RegisterClassExA
.extern CreateWindowExA
.extern ShowWindow
.extern UpdateWindow
.extern GetMessageA
.extern TranslateMessage
.extern DispatchMessageA
.extern PostQuitMessage
.extern DefWindowProcA
.extern BeginPaint
.extern EndPaint
.extern InvalidateRect
.extern CreateSolidBrush
.extern SelectObject
.extern DeleteObject
.extern FillRect
.extern SetBkMode
.extern SetTextColor
.extern TextOutA
.extern CreateFontA
.extern GetOpenFileNameA
.extern ShellExecuteA
.extern GetFileAttributesA
.extern FindFirstFileA
.extern FindClose
.extern CreateDirectoryA
.extern CreateFileA
.extern SetFilePointer
.extern ReadFile
.extern WriteFile
.extern CloseHandle
.extern ExitProcess

# ==============================================================================
# Helper: my_strlen(rcx = str) -> rax = length
# ==============================================================================
my_strlen:
    xor eax, eax
    test rcx, rcx
    jz .Lstrlen_done
.Lstrlen_loop:
    cmp byte ptr [rcx + rax], 0
    je .Lstrlen_done
    inc rax
    jmp .Lstrlen_loop
.Lstrlen_done:
    ret

# ==============================================================================
# Helper: my_strcpy(rcx = dst, rdx = src)
# ==============================================================================
my_strcpy:
    xor eax, eax
.Lstrcpy_loop:
    mov r8b, byte ptr [rdx + rax]
    mov byte ptr [rcx + rax], r8b
    test r8b, r8b
    jz .Lstrcpy_done
    inc rax
    jmp .Lstrcpy_loop
.Lstrcpy_done:
    ret

# ==============================================================================
# Helper: my_strcat(rcx = dst, rdx = src)
# ==============================================================================
my_strcat:
    push rdi
    push rsi
    sub rsp, 40
    mov rdi, rcx
    mov rsi, rdx
    call my_strlen
    add rdi, rax
    mov rcx, rdi
    mov rdx, rsi
    call my_strcpy
    add rsp, 40
    pop rsi
    pop rdi
    ret

# ==============================================================================
# Helper: aes_ctr_encrypt_buf(rcx = buf_ptr, rdx = byte_len)
# Hardware x86_64 AES-NI AES-128-CTR encryption for Switch NCA Section 1 (RomFS)
# ==============================================================================
aes_ctr_encrypt_buf:
    test rdx, rdx
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

    movdqu xmm1, xmmword ptr [rcx]
    pxor xmm0, xmm1
    movdqu xmmword ptr [rcx], xmm0

    add rcx, 16
    sub rdx, 16
    jnz .Laes_loop
    mov qword ptr [rip + g_aes_ctr], r9
.Laes_done:
    ret

# ==============================================================================
# Helper: win_read_all(rcx = hFile, rdx = buf, r8d = len) -> eax (0 = ok, -1 = err)
# ==============================================================================
win_read_all:
    push rbp
    mov rbp, rsp
    push rbx
    push rsi
    push rdi
    sub rsp, 64

    mov rbx, rcx                  # hFile
    mov rsi, rdx                  # buf
    mov edi, r8d                  # remaining

.Lwra_loop:
    test edi, edi
    jz .Lwra_ok
    mov qword ptr [rsp + 32], 0
    lea r9, [rsp + 48]            # &bytesRead
    mov r8d, edi
    mov rdx, rsi
    mov rcx, rbx
    call ReadFile
    test eax, eax
    jz .Lwra_err
    mov eax, dword ptr [rsp + 48]
    test eax, eax
    jz .Lwra_err
    add rsi, rax
    sub edi, eax
    jmp .Lwra_loop

.Lwra_ok:
    xor eax, eax
    jmp .Lwra_ret
.Lwra_err:
    mov eax, -1
.Lwra_ret:
    add rsp, 64
    pop rdi
    pop rsi
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: win_write_all(rcx = hFile, rdx = buf, r8d = len) -> eax (0 = ok, -1 = err)
# ==============================================================================
win_write_all:
    push rbp
    mov rbp, rsp
    push rbx
    push rsi
    push rdi
    sub rsp, 64

    mov rbx, rcx                  # hFile
    mov rsi, rdx                  # buf
    mov edi, r8d                  # remaining

.Lwwa_loop:
    test edi, edi
    jz .Lwwa_ok
    mov qword ptr [rsp + 32], 0
    lea r9, [rsp + 48]            # &bytesWritten
    mov r8d, edi
    mov rdx, rsi
    mov rcx, rbx
    call WriteFile
    test eax, eax
    jz .Lwwa_err
    mov eax, dword ptr [rsp + 48]
    test eax, eax
    jz .Lwwa_err
    add rsi, rax
    sub edi, eax
    jmp .Lwwa_loop

.Lwwa_ok:
    xor eax, eax
    jmp .Lwwa_ret
.Lwwa_err:
    mov eax, -1
.Lwwa_ret:
    add rsp, 64
    pop rdi
    pop rsi
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: out_flush() -> eax (0 = ok, -1 = err)
# ==============================================================================
out_flush:
    push rbp
    mov rbp, rsp
    sub rsp, 32

    mov edx, dword ptr [rip + g_out_pos]
    test edx, edx
    jz .Lof_ok

    cmp dword ptr [rip + g_is_nsp], 0
    je .Lof_write
    lea rcx, [rip + g_out_buf]
    call aes_ctr_encrypt_buf

.Lof_write:
    mov rcx, qword ptr [rip + g_out_handle]
    lea rdx, [rip + g_out_buf]
    mov r8d, dword ptr [rip + g_out_pos]
    mov dword ptr [rip + g_out_pos], 0
    call win_write_all
    add rsp, 32
    pop rbp
    ret

.Lof_ok:
    xor eax, eax
    add rsp, 32
    pop rbp
    ret

# ==============================================================================
# Helper: out_emit(rcx = src_ptr, edx = len) -> eax (0 = ok, -1 = err)
# ==============================================================================
out_emit:
    push rbp
    mov rbp, rsp
    push rbx
    push rsi
    push rdi
    push r12
    push r13
    sub rsp, 40

    mov r12, rcx                  # src_ptr
    mov r13d, edx                 # remaining len

.Loe_loop:
    test r13d, r13d
    jz .Loe_ok

    mov eax, 65536
    mov edx, dword ptr [rip + g_out_pos]
    sub eax, edx
    mov ebx, r13d
    cmp ebx, eax
    cmova ebx, eax                # chunk = min(rem, space)

    lea rdi, [rip + g_out_buf]
    add rdi, rdx
    mov rsi, r12
    mov ecx, ebx
    rep movsb

    add dword ptr [rip + g_out_pos], ebx
    add r12, rbx
    sub r13d, ebx

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
    add rsp, 40
    pop r13
    pop r12
    pop rdi
    pop rsi
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: copy_handle_range(rcx = hIn, rdx = hOut, r8d = byte_count) -> eax
# ==============================================================================
copy_handle_range:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    push r13
    push r14
    sub rsp, 32

    mov rbx, rcx                  # hIn
    mov r12, rdx                  # hOut
    mov r13d, r8d                 # remaining

.Lchr_loop:
    test r13d, r13d
    jz .Lchr_ok
    mov r14d, 65536
    cmp r13d, r14d
    cmovb r14d, r13d

    mov rcx, rbx
    lea rdx, [rip + g_io_buf]
    mov r8d, r14d
    call win_read_all
    test eax, eax
    jnz .Lchr_err

    mov rcx, r12
    lea rdx, [rip + g_io_buf]
    mov r8d, r14d
    call win_write_all
    test eax, eax
    jnz .Lchr_err

    sub r13d, r14d
    jmp .Lchr_loop

.Lchr_ok:
    xor eax, eax
    jmp .Lchr_ret
.Lchr_err:
    mov eax, -1
.Lchr_ret:
    add rsp, 32
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: refresh_gui_now()
# ==============================================================================
refresh_gui_now:
    push rbp
    mov rbp, rsp
    sub rsp, 32
    mov rcx, qword ptr [rip + g_hwnd]
    test rcx, rcx
    jz .Lrgn_done
    xor r8d, r8d
    xor edx, edx
    call InvalidateRect
    mov rcx, qword ptr [rip + g_hwnd]
    call UpdateWindow
.Lrgn_done:
    add rsp, 32
    pop rbp
    ret

# ==============================================================================
# Helper: open_and_validate_iso() -> rax (HANDLE or -1)
# ==============================================================================
open_and_validate_iso:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    sub rsp, 64

    cmp byte ptr [rip + g_iso_path], 0
    jne .Loav_open
    mov rcx, qword ptr [rip + g_hwnd]
    call browse_iso_dialog
    cmp byte ptr [rip + g_iso_path], 0
    je .Loav_no_iso

.Loav_open:
    mov qword ptr [rsp + 48], 0
    mov dword ptr [rsp + 40], FILE_ATTRIBUTE_NORMAL
    mov dword ptr [rsp + 32], OPEN_EXISTING
    xor r9d, r9d
    mov r8d, FILE_SHARE_READ
    mov edx, GENERIC_READ
    lea rcx, [rip + g_iso_path]
    call CreateFileA
    cmp rax, -1
    je .Loav_open_err
    mov rbx, rax                  # hIso

    mov rcx, rbx
    lea rdx, [rip + g_io_buf]
    mov r8d, 0x440
    call win_read_all
    test eax, eax
    jnz .Loav_bad_iso

    # Verify GameCube magic at +0x1C (0xC2339F3D BE == 0x3D9F33C2 LE)
    mov eax, dword ptr [rip + g_io_buf + 0x1C]
    cmp eax, 0x3D9F33C2
    jne .Loav_bad_iso

    mov edx, dword ptr [rip + g_io_buf + 0x424]
    bswap edx                     # fst_off
    mov r12d, dword ptr [rip + g_io_buf + 0x428]
    bswap r12d                    # fst_size
    cmp r12d, 8192
    ja .Loav_bad_iso

    mov rcx, rbx
    xor r8d, r8d
    mov r9d, FILE_BEGIN
    call SetFilePointer
    cmp eax, -1
    je .Loav_bad_iso

    mov rcx, rbx
    lea rdx, [rip + g_fst_buf]
    mov r8d, r12d
    call win_read_all
    test eax, eax
    jnz .Loav_bad_iso

    mov qword ptr [rip + g_iso_handle], rbx
    mov rax, rbx
    jmp .Loav_ret

.Loav_no_iso:
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_err_no_iso]
    call my_strcpy
    mov rax, -1
    jmp .Loav_ret

.Loav_open_err:
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_err_open_iso]
    call my_strcpy
    mov rax, -1
    jmp .Loav_ret

.Loav_bad_iso:
    mov rcx, rbx
    call CloseHandle
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_err_bad_iso]
    call my_strcpy
    mov rax, -1

.Loav_ret:
    add rsp, 64
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: stream_romfs_from_iso() -> eax (0 = ok, -1 = err)
# ==============================================================================
stream_romfs_from_iso:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    push r13
    push r14
    push r15
    sub rsp, 40

    mov dword ptr [rip + g_out_pos], 0
    mov rax, 0x00147240
    mov qword ptr [rip + g_aes_ctr], rax

    lea rcx, [rip + romfs_prefix_560]
    mov edx, 560
    call out_emit
    test eax, eax
    jnz .Lsr_err

    xor r12d, r12d
    lea r13, [rip + romfs_iso_table]

.Lsr_file_loop:
    cmp r12d, 186
    jge .Lsr_footer

    mov eax, dword ptr [r13]      # fst_idx
    mov r14d, dword ptr [r13 + 4] # file_size
    mov r15d, dword ptr [r13 + 8] # pad_len

    imul eax, 12
    lea rcx, [rip + g_fst_buf]
    mov edx, dword ptr [rcx + rax + 4]
    bswap edx                     # iso_offset

    mov rcx, qword ptr [rip + g_iso_handle]
    xor r8d, r8d
    mov r9d, FILE_BEGIN
    call SetFilePointer
    cmp eax, -1
    je .Lsr_err

.Lsr_chunk_loop:
    test r14d, r14d
    jz .Lsr_pad
    mov ebx, 65536
    cmp r14d, ebx
    cmovb ebx, r14d

    mov rcx, qword ptr [rip + g_iso_handle]
    lea rdx, [rip + g_io_buf]
    mov r8d, ebx
    call win_read_all
    test eax, eax
    jnz .Lsr_err

    lea rcx, [rip + g_io_buf]
    mov edx, ebx
    call out_emit
    test eax, eax
    jnz .Lsr_err

    sub r14d, ebx
    jmp .Lsr_chunk_loop

.Lsr_pad:
    test r15d, r15d
    jz .Lsr_next_file
    lea rcx, [rip + zero_pad_16]
    mov edx, r15d
    call out_emit
    test eax, eax
    jnz .Lsr_err

.Lsr_next_file:
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
    call refresh_gui_now

.Lsr_step:
    inc r12d
    add r13, 12
    jmp .Lsr_file_loop

.Lsr_footer:
    lea rcx, [rip + romfs_footer_9348]
    mov edx, 9348
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
    add rsp, 40
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Native x86_64 NRO Builder: build_nro_native() -> eax (0 = ok, -1 = err)
# Produces target\switch\tssm.nro (782,999,087 bytes)
# ==============================================================================
build_nro_native:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    sub rsp, 64

    call open_and_validate_iso
    cmp rax, -1
    je .Lbnro_fail

    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_status_building_nro]
    call my_strcpy
    mov dword ptr [rip + g_progress_pct], 5
    call refresh_gui_now

    lea rcx, [rip + str_dir_target]
    xor edx, edx
    call CreateDirectoryA
    lea rcx, [rip + str_dir_switch]
    xor edx, edx
    call CreateDirectoryA

    # Open tools\switch\tssm_engine.nro
    mov qword ptr [rsp + 48], 0
    mov dword ptr [rsp + 40], FILE_ATTRIBUTE_NORMAL
    mov dword ptr [rsp + 32], OPEN_EXISTING
    xor r9d, r9d
    mov r8d, FILE_SHARE_READ
    mov edx, GENERIC_READ
    lea rcx, [rip + str_path_engine_nro]
    call CreateFileA
    cmp rax, -1
    je .Lbnro_no_tpl
    mov rbx, rax                  # hTpl

    # Create target\switch\tssm.nro
    mov qword ptr [rsp + 48], 0
    mov dword ptr [rsp + 40], FILE_ATTRIBUTE_NORMAL
    mov dword ptr [rsp + 32], CREATE_ALWAYS
    xor r9d, r9d
    xor r8d, r8d
    mov edx, GENERIC_WRITE
    lea rcx, [rip + str_path_out_nro]
    call CreateFileA
    cmp rax, -1
    je .Lbnro_close_tpl
    mov r12, rax                  # hOut
    mov qword ptr [rip + g_out_handle], r12

    mov rcx, rbx
    mov rdx, r12
    mov r8d, 31329551
    call copy_handle_range
    push rax
    mov rcx, rbx
    call CloseHandle
    pop rax
    test eax, eax
    jnz .Lbnro_write_err

    mov dword ptr [rip + g_is_nsp], 0
    call stream_romfs_from_iso
    mov ebx, eax

    mov rcx, r12
    call CloseHandle
    mov rcx, qword ptr [rip + g_iso_handle]
    call CloseHandle

    test ebx, ebx
    jnz .Lbnro_write_err

    mov dword ptr [rip + g_progress_pct], 100
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_status_nro_ok]
    call my_strcpy
    call refresh_gui_now
    xor eax, eax
    jmp .Lbnro_ret

.Lbnro_close_tpl:
    mov rcx, rbx
    call CloseHandle
.Lbnro_no_tpl:
    mov rcx, qword ptr [rip + g_iso_handle]
    call CloseHandle
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_err_tpl_nro]
    call my_strcpy
    mov eax, -1
    jmp .Lbnro_ret

.Lbnro_write_err:
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_err_write]
    call my_strcpy
.Lbnro_fail:
    mov eax, -1
.Lbnro_ret:
    call refresh_gui_now
    add rsp, 64
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Native x86_64 NSP Builder: build_nsp_native() -> eax (0 = ok, -1 = err)
# Produces target\switch\nsp\0100780012340000.nsp (773,292,248 bytes)
# ==============================================================================
build_nsp_native:
    push rbp
    mov rbp, rsp
    push rbx
    push r12
    sub rsp, 64

    call open_and_validate_iso
    cmp rax, -1
    je .Lbnsp_fail

    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_status_building_nsp]
    call my_strcpy
    mov dword ptr [rip + g_progress_pct], 5
    call refresh_gui_now

    lea rcx, [rip + str_dir_target]
    xor edx, edx
    call CreateDirectoryA
    lea rcx, [rip + str_dir_switch]
    xor edx, edx
    call CreateDirectoryA
    lea rcx, [rip + str_dir_nsp]
    xor edx, edx
    call CreateDirectoryA

    # Open tools\switch\tssm_nsp_template.bin
    mov qword ptr [rsp + 48], 0
    mov dword ptr [rsp + 40], FILE_ATTRIBUTE_NORMAL
    mov dword ptr [rsp + 32], OPEN_EXISTING
    xor r9d, r9d
    mov r8d, FILE_SHARE_READ
    mov edx, GENERIC_READ
    lea rcx, [rip + str_path_nsp_tpl]
    call CreateFileA
    cmp rax, -1
    je .Lbnsp_no_tpl
    mov rbx, rax                  # hTpl

    # Create target\switch\nsp\0100780012340000.nsp
    mov qword ptr [rsp + 48], 0
    mov dword ptr [rsp + 40], FILE_ATTRIBUTE_NORMAL
    mov dword ptr [rsp + 32], CREATE_ALWAYS
    xor r9d, r9d
    xor r8d, r8d
    mov edx, GENERIC_WRITE
    lea rcx, [rip + str_path_out_nsp]
    call CreateFileA
    cmp rax, -1
    je .Lbnsp_close_tpl
    mov r12, rax                  # hOut
    mov qword ptr [rip + g_out_handle], r12

    # 1. Copy 21,439,704-byte NSP prefix
    mov rcx, rbx
    mov rdx, r12
    mov r8d, 21439704
    call copy_handle_range
    test eax, eax
    jnz .Lbnsp_err_close

    # 2. Stream & AES-128-CTR encrypt 751,669,536-byte RomFS Level 6
    mov dword ptr [rip + g_is_nsp], 1
    call stream_romfs_from_iso
    test eax, eax
    jnz .Lbnsp_err_close

    # 3. Copy 183,008-byte NSP suffix
    mov rcx, rbx
    mov rdx, r12
    mov r8d, 183008
    call copy_handle_range
    test eax, eax
    jnz .Lbnsp_err_close

    mov rcx, rbx
    call CloseHandle
    mov rcx, r12
    call CloseHandle
    mov rcx, qword ptr [rip + g_iso_handle]
    call CloseHandle

    mov dword ptr [rip + g_progress_pct], 100
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_status_nsp_ok]
    call my_strcpy
    call refresh_gui_now
    xor eax, eax
    jmp .Lbnsp_ret

.Lbnsp_err_close:
    mov rcx, rbx
    call CloseHandle
    mov rcx, r12
    call CloseHandle
    mov rcx, qword ptr [rip + g_iso_handle]
    call CloseHandle
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_err_write]
    call my_strcpy
    mov eax, -1
    jmp .Lbnsp_ret

.Lbnsp_close_tpl:
    mov rcx, rbx
    call CloseHandle
.Lbnsp_no_tpl:
    mov rcx, qword ptr [rip + g_iso_handle]
    call CloseHandle
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_err_tpl_nsp]
    call my_strcpy
.Lbnsp_fail:
    mov eax, -1
.Lbnsp_ret:
    call refresh_gui_now
    add rsp, 64
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: draw_text(rcx = hdc, edx = x, r8d = y, r9 = str, [rsp+40] = color, [rsp+48] = font)
# ==============================================================================
draw_text:
    push rbp
    mov rbp, rsp
    push rbx
    push rsi
    push rdi
    push r12
    sub rsp, 48

    mov rbx, rcx                  # hdc
    mov esi, edx                  # x
    mov edi, r8d                  # y
    mov r12, r9                   # str

    mov rdx, qword ptr [rbp + 56] # hFont
    test rdx, rdx
    jz .Ldt_no_font
    mov rcx, rbx
    call SelectObject
.Ldt_no_font:
    mov rcx, rbx
    mov edx, dword ptr [rbp + 48] # color
    call SetTextColor

    mov rcx, r12
    call my_strlen
    mov dword ptr [rsp + 32], eax # len (5th arg)
    mov r9, r12                   # lpString (4th arg)
    mov r8d, edi                  # y (3rd arg)
    mov edx, esi                  # x (2nd arg)
    mov rcx, rbx                  # hdc (1st arg)
    call TextOutA

    add rsp, 48
    pop r12
    pop rdi
    pop rsi
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: fill_box(rcx = hdc, edx = left, r8d = top, r9d = right, [rsp+40] = bottom, [rsp+48] = color)
# ==============================================================================
fill_box:
    push rbp
    mov rbp, rsp
    push rbx
    push rsi
    sub rsp, 64

    mov rbx, rcx
    mov dword ptr [rsp + 32], edx
    mov dword ptr [rsp + 36], r8d
    mov dword ptr [rsp + 40], r9d
    mov eax, dword ptr [rbp + 48]
    mov dword ptr [rsp + 44], eax

    mov ecx, dword ptr [rbp + 56]
    call CreateSolidBrush
    mov rsi, rax

    mov rcx, rbx
    lea rdx, [rsp + 32]
    mov r8, rsi
    call FillRect

    mov rcx, rsi
    call DeleteObject

    add rsp, 64
    pop rsi
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: draw_button(rcx = hdc, edx = btn_id, r8d = left, r9d = top,
#                     [rsp+40] = right, [rsp+48] = bottom, [rsp+56] = bg_col,
#                     [rsp+64] = hover_col, [rsp+72] = text_col, [rsp+80] = label,
#                     [rsp+88] = text_offset_x)
# ==============================================================================
draw_button:
    push rbp
    mov rbp, rsp
    push rbx
    push rsi
    push rdi
    push r12
    push r13
    push r14
    sub rsp, 64

    mov rbx, rcx                  # hdc
    mov esi, edx                  # btn_id
    mov edi, r8d                  # left
    mov r12d, r9d                 # top
    mov r13d, dword ptr [rbp + 48]# right
    mov r14d, dword ptr [rbp + 56]# bottom

    mov eax, dword ptr [rbp + 64] # bg_col
    cmp esi, dword ptr [rip + g_hover_btn]
    jne .Lbtn_col_ready
    mov eax, dword ptr [rbp + 72] # hover_col
.Lbtn_col_ready:
    mov dword ptr [rsp + 40], eax
    mov dword ptr [rsp + 32], r14d
    mov r9d, r13d
    mov r8d, r12d
    mov edx, edi
    mov rcx, rbx
    call fill_box

    cmp esi, dword ptr [rip + g_hover_btn]
    jne .Lbtn_draw_label
    mov dword ptr [rsp + 40], COL_TEXT_WHITE
    lea eax, [r12d + 2]
    mov dword ptr [rsp + 32], eax
    mov r9d, r13d
    mov r8d, r12d
    mov edx, edi
    mov rcx, rbx
    call fill_box

.Lbtn_draw_label:
    mov rax, qword ptr [rip + g_font_btn]
    mov qword ptr [rsp + 40], rax
    mov eax, dword ptr [rbp + 80] # text_col
    mov dword ptr [rsp + 32], eax
    mov r9, qword ptr [rbp + 88]  # label
    mov eax, r14d
    sub eax, r12d
    shr eax, 1
    lea r8d, [r12 + rax - 9]
    mov edx, edi
    add edx, dword ptr [rbp + 96]
    mov rcx, rbx
    call draw_text

    add rsp, 64
    pop r14
    pop r13
    pop r12
    pop rdi
    pop rsi
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Helper: point_in_rect(ecx = mx, edx = my, r8d = l, r9d = t, [rsp+40] = r, [rsp+48] = b) -> eax (0/1)
# ==============================================================================
point_in_rect:
    xor eax, eax
    cmp ecx, r8d
    jl .Lpir_out
    cmp ecx, dword ptr [rsp + 40]
    jge .Lpir_out
    cmp edx, r9d
    jl .Lpir_out
    cmp edx, dword ptr [rsp + 48]
    jge .Lpir_out
    mov eax, 1
.Lpir_out:
    ret

# ==============================================================================
# Helper: hit_test_buttons(ecx = mx, edx = my) -> eax = btn_id (0 if none)
# ==============================================================================
hit_test_buttons:
    push rbx
    push rsi
    sub rsp, 56
    mov ebx, ecx
    mov esi, edx

    # Button 1: Browse ISO (590, 118, 718, 152)
    mov dword ptr [rsp + 40], 152
    mov dword ptr [rsp + 32], 718
    mov r9d, 118
    mov r8d, 590
    mov edx, esi
    mov ecx, ebx
    call point_in_rect
    test eax, eax
    jnz .Lht_1

    # Toggle 2: Release vs Dev (44, 210, 360, 240)
    mov dword ptr [rsp + 40], 240
    mov dword ptr [rsp + 32], 360
    mov r9d, 210
    mov r8d, 44
    mov edx, esi
    mov ecx, ebx
    call point_in_rect
    test eax, eax
    jnz .Lht_2

    # Button 3: Build Windows PC Port (.exe) (390, 210, 716, 242)
    mov dword ptr [rsp + 40], 242
    mov dword ptr [rsp + 32], 716
    mov r9d, 210
    mov r8d, 390
    mov edx, esi
    mov ecx, ebx
    call point_in_rect
    test eax, eax
    jnz .Lht_3

    # Button 4: Build Switch (.NRO + .NSP) (44, 304, 366, 356)
    mov dword ptr [rsp + 40], 356
    mov dword ptr [rsp + 32], 366
    mov r9d, 304
    mov r8d, 44
    mov edx, esi
    mov ecx, ebx
    call point_in_rect
    test eax, eax
    jnz .Lht_4

    # Button 5: Build Switch .NSP Only (394, 304, 716, 356)
    mov dword ptr [rsp + 40], 356
    mov dword ptr [rsp + 32], 716
    mov r9d, 304
    mov r8d, 394
    mov edx, esi
    mov ecx, ebx
    call point_in_rect
    test eax, eax
    jnz .Lht_5

    # Button 6: Build Switch .NRO Only (44, 370, 366, 416)
    mov dword ptr [rsp + 40], 416
    mov dword ptr [rsp + 32], 366
    mov r9d, 370
    mov r8d, 44
    mov edx, esi
    mov ecx, ebx
    call point_in_rect
    test eax, eax
    jnz .Lht_6

    # Button 7: Open Output Folder (394, 370, 716, 416)
    mov dword ptr [rsp + 40], 416
    mov dword ptr [rsp + 32], 716
    mov r9d, 370
    mov r8d, 394
    mov edx, esi
    mov ecx, ebx
    call point_in_rect
    test eax, eax
    jnz .Lht_7

    xor eax, eax
    jmp .Lht_done
.Lht_1: mov eax, 1; jmp .Lht_done
.Lht_2: mov eax, 2; jmp .Lht_done
.Lht_3: mov eax, 3; jmp .Lht_done
.Lht_4: mov eax, 4; jmp .Lht_done
.Lht_5: mov eax, 5; jmp .Lht_done
.Lht_6: mov eax, 6; jmp .Lht_done
.Lht_7: mov eax, 7
.Lht_done:
    add rsp, 56
    pop rsi
    pop rbx
    ret

# ==============================================================================
# Helper: browse_iso_dialog(rcx = hwnd)
# ==============================================================================
browse_iso_dialog:
    push rbp
    mov rbp, rsp
    push rdi
    sub rsp, 192

    lea rdi, [rsp + 32]
    xor eax, eax
    mov ecx, 152
    rep stosb

    mov dword ptr [rsp + 32], 152
    mov rax, qword ptr [rip + g_hwnd]
    mov qword ptr [rsp + 40], rax
    lea rax, [rip + str_ofn_filter]
    mov qword ptr [rsp + 56], rax
    lea rax, [rip + g_iso_path]
    mov qword ptr [rsp + 80], rax
    mov dword ptr [rsp + 88], 512
    lea rax, [rip + str_ofn_title]
    mov qword ptr [rsp + 112], rax
    mov dword ptr [rsp + 120], (OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR)

    lea rcx, [rsp + 32]
    call GetOpenFileNameA
    test eax, eax
    jz .Lbrowse_cancel

    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_status_iso_set]
    call my_strcpy
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + g_iso_path]
    call my_strcat
    mov dword ptr [rip + g_progress_pct], 25

.Lbrowse_cancel:
    add rsp, 192
    pop rdi
    pop rbp
    ret

# ==============================================================================
# Helper: run_windows_build(rcx = hwnd)
# ==============================================================================
run_windows_build:
    push rbp
    mov rbp, rsp
    push rbx
    sub rsp, 64
    mov rbx, rcx

    lea rcx, [rip + g_cmd_args]
    lea rdx, [rip + str_ps_base]
    call my_strcpy

    cmp byte ptr [rip + g_iso_path], 0
    je .Lwin_chk_dev
    lea rcx, [rip + g_cmd_args]
    lea rdx, [rip + str_ps_iso_prefix]
    call my_strcat
    lea rcx, [rip + g_cmd_args]
    lea rdx, [rip + g_iso_path]
    call my_strcat
    lea rcx, [rip + g_cmd_args]
    lea rdx, [rip + str_quote]
    call my_strcat

.Lwin_chk_dev:
    cmp dword ptr [rip + g_opt_release], 0
    jne .Lwin_exec
    lea rcx, [rip + g_cmd_args]
    lea rdx, [rip + str_ps_dev]
    call my_strcat

.Lwin_exec:
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_status_building_win]
    call my_strcpy
    mov dword ptr [rip + g_progress_pct], 100

    mov dword ptr [rsp + 40], SW_SHOWNORMAL
    mov qword ptr [rsp + 32], 0
    lea r9, [rip + g_cmd_args]
    lea r8, [rip + str_powershell]
    lea rdx, [rip + str_open]
    mov rcx, rbx
    call ShellExecuteA

    add rsp, 64
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Window Procedure: WndProc(rcx = hwnd, edx = msg, r8 = wParam, r9 = lParam)
# ==============================================================================
WndProc:
    push rbp
    mov rbp, rsp
    push rbx
    push rsi
    push rdi
    sub rsp, 144

    mov rbx, rcx                  # hwnd
    mov esi, edx                  # msg

    cmp esi, WM_CREATE
    je .Lwm_create
    cmp esi, WM_ERASEBKGND
    je .Lwm_erase
    cmp esi, WM_MOUSEMOVE
    je .Lwm_mousemove
    cmp esi, WM_LBUTTONDOWN
    je .Lwm_lbuttondown
    cmp esi, WM_PAINT
    je .Lwm_paint
    cmp esi, WM_DESTROY
    je .Lwm_destroy

    call DefWindowProcA
    jmp .Lwnd_ret

.Lwm_erase:
    mov eax, 1
    jmp .Lwnd_ret

.Lwm_create:
    mov qword ptr [rip + g_hwnd], rbx

    lea rax, [rip + str_font_segoe]
    mov qword ptr [rsp + 96], rax
    mov dword ptr [rsp + 88], 0
    mov dword ptr [rsp + 80], CLEARTYPE_QUALITY
    mov dword ptr [rsp + 72], 0
    mov dword ptr [rsp + 64], 0
    mov dword ptr [rsp + 56], DEFAULT_CHARSET
    mov dword ptr [rsp + 48], 0
    mov dword ptr [rsp + 40], 0
    mov dword ptr [rsp + 32], FW_BOLD
    xor r9d, r9d
    xor r8d, r8d
    xor edx, edx
    mov ecx, -21
    call CreateFontA
    mov qword ptr [rip + g_font_title], rax

    lea rax, [rip + str_font_segoe]
    mov qword ptr [rsp + 96], rax
    mov dword ptr [rsp + 88], 0
    mov dword ptr [rsp + 80], CLEARTYPE_QUALITY
    mov dword ptr [rsp + 72], 0
    mov dword ptr [rsp + 64], 0
    mov dword ptr [rsp + 56], DEFAULT_CHARSET
    mov dword ptr [rsp + 48], 0
    mov dword ptr [rsp + 40], 0
    mov dword ptr [rsp + 32], FW_SEMIBOLD
    xor r9d, r9d
    xor r8d, r8d
    xor edx, edx
    mov ecx, -15
    call CreateFontA
    mov qword ptr [rip + g_font_btn], rax

    lea rax, [rip + str_font_segoe]
    mov qword ptr [rsp + 96], rax
    mov dword ptr [rsp + 88], 0
    mov dword ptr [rsp + 80], CLEARTYPE_QUALITY
    mov dword ptr [rsp + 72], 0
    mov dword ptr [rsp + 64], 0
    mov dword ptr [rsp + 56], DEFAULT_CHARSET
    mov dword ptr [rsp + 48], 0
    mov dword ptr [rsp + 40], 0
    mov dword ptr [rsp + 32], FW_NORMAL
    xor r9d, r9d
    xor r8d, r8d
    xor edx, edx
    mov ecx, -14
    call CreateFontA
    mov qword ptr [rip + g_font_body], rax

    # Auto-detect any *.iso or *.gcm in current folder via FindFirstFileA
    lea rcx, [rip + str_glob_iso]
    lea rdx, [rip + g_find_data]
    call FindFirstFileA
    cmp rax, -1
    jne .Lwm_found_iso

    lea rcx, [rip + str_glob_gcm]
    lea rdx, [rip + g_find_data]
    call FindFirstFileA
    cmp rax, -1
    je .Lwm_create_done

.Lwm_found_iso:
    mov rcx, rax
    call FindClose
    lea rcx, [rip + g_iso_path]
    lea rdx, [rip + g_find_data + 44]
    call my_strcpy
    mov dword ptr [rip + g_progress_pct], 25

.Lwm_create_done:
    xor eax, eax
    jmp .Lwnd_ret

.Lwm_mousemove:
    movzx ecx, r9w
    shr r9, 16
    movzx edx, r9w
    call hit_test_buttons
    cmp eax, dword ptr [rip + g_hover_btn]
    je .Lmm_no_change
    mov dword ptr [rip + g_hover_btn], eax
    xor r8d, r8d
    xor edx, edx
    mov rcx, rbx
    call InvalidateRect
.Lmm_no_change:
    xor eax, eax
    jmp .Lwnd_ret

.Lwm_lbuttondown:
    movzx ecx, r9w
    shr r9, 16
    movzx edx, r9w
    call hit_test_buttons
    cmp eax, 1
    je .Lclick_browse
    cmp eax, 2
    je .Lclick_toggle_rel
    cmp eax, 3
    je .Lclick_build_win
    cmp eax, 4
    je .Lclick_build_both
    cmp eax, 5
    je .Lclick_build_nsp
    cmp eax, 6
    je .Lclick_build_nro
    cmp eax, 7
    je .Lclick_open_dir
    xor eax, eax
    jmp .Lwnd_ret

.Lclick_browse:
    mov rcx, rbx
    call browse_iso_dialog
    jmp .Lclick_refresh

.Lclick_toggle_rel:
    xor dword ptr [rip + g_opt_release], 1
    jmp .Lclick_refresh

.Lclick_build_win:
    mov rcx, rbx
    call run_windows_build
    jmp .Lclick_refresh

.Lclick_build_both:
    call build_nro_native
    test eax, eax
    jnz .Lclick_refresh
    call build_nsp_native
    test eax, eax
    jnz .Lclick_refresh
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_status_both_ok]
    call my_strcpy
    jmp .Lclick_refresh

.Lclick_build_nsp:
    call build_nsp_native
    jmp .Lclick_refresh

.Lclick_build_nro:
    call build_nro_native
    jmp .Lclick_refresh

.Lclick_open_dir:
    lea rcx, [rip + str_dir_target]
    xor edx, edx
    call CreateDirectoryA
    lea rcx, [rip + str_dir_switch]
    xor edx, edx
    call CreateDirectoryA

    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_status_opened_dir]
    call my_strcpy
    mov dword ptr [rsp + 40], SW_SHOWNORMAL
    mov qword ptr [rsp + 32], 0
    xor r9d, r9d
    lea r8, [rip + str_dir_switch]
    lea rdx, [rip + str_open]
    mov rcx, rbx
    call ShellExecuteA

.Lclick_refresh:
    xor r8d, r8d
    xor edx, edx
    mov rcx, rbx
    call InvalidateRect
    xor eax, eax
    jmp .Lwnd_ret

.Lwm_paint:
    lea rdx, [rsp + 64]
    mov rcx, rbx
    call BeginPaint
    mov rdi, rax

    mov rcx, rdi
    mov edx, TRANSPARENT
    call SetBkMode

    # 1. Main window background (0, 0, 760, 520)
    mov dword ptr [rsp + 40], COL_BG_DARK
    mov dword ptr [rsp + 32], 520
    mov r9d, 760
    xor r8d, r8d
    xor edx, edx
    mov rcx, rdi
    call fill_box

    # 2. Top Header Banner (0, 0, 760, 76) + Gold accent bar (0, 74, 760, 77)
    mov dword ptr [rsp + 40], COL_HEADER_BG
    mov dword ptr [rsp + 32], 74
    mov r9d, 760
    xor r8d, r8d
    xor edx, edx
    mov rcx, rdi
    call fill_box

    mov dword ptr [rsp + 40], COL_GOLD
    mov dword ptr [rsp + 32], 77
    mov r9d, 760
    mov r8d, 74
    xor edx, edx
    mov rcx, rdi
    call fill_box

    mov rax, qword ptr [rip + g_font_title]
    mov qword ptr [rsp + 40], rax
    mov dword ptr [rsp + 32], COL_GOLD
    lea r9, [rip + str_header_title]
    mov r8d, 14
    mov edx, 28
    mov rcx, rdi
    call draw_text

    mov rax, qword ptr [rip + g_font_body]
    mov qword ptr [rsp + 40], rax
    mov dword ptr [rsp + 32], COL_TEXT_MUTED
    lea r9, [rip + str_header_sub]
    mov r8d, 44
    mov edx, 28
    mov rcx, rdi
    call draw_text

    # 3. Card 1: GameCube Disc Image (24, 92, 736, 168)
    mov dword ptr [rsp + 40], COL_CARD_BORDER
    mov dword ptr [rsp + 32], 169
    mov r9d, 737
    mov r8d, 91
    mov edx, 23
    mov rcx, rdi
    call fill_box

    mov dword ptr [rsp + 40], COL_CARD_BG
    mov dword ptr [rsp + 32], 168
    mov r9d, 736
    mov r8d, 92
    mov edx, 24
    mov rcx, rdi
    call fill_box

    mov rax, qword ptr [rip + g_font_btn]
    mov qword ptr [rsp + 40], rax
    mov dword ptr [rsp + 32], COL_TEXT_WHITE
    lea r9, [rip + str_card1_title]
    mov r8d, 98
    mov edx, 40
    mov rcx, rdi
    call draw_text

    # ISO path box (40, 120, 578, 150)
    mov dword ptr [rsp + 40], COL_INPUT_BG
    mov dword ptr [rsp + 32], 150
    mov r9d, 578
    mov r8d, 120
    mov edx, 40
    mov rcx, rdi
    call fill_box

    mov rax, qword ptr [rip + g_font_body]
    mov qword ptr [rsp + 40], rax
    lea r9, [rip + g_iso_path]
    mov dword ptr [rsp + 32], COL_GREEN
    cmp byte ptr [r9], 0
    jne .Liso_has_path
    lea r9, [rip + str_iso_placeholder]
    mov dword ptr [rsp + 32], COL_TEXT_MUTED
.Liso_has_path:
    mov r8d, 126
    mov edx, 50
    mov rcx, rdi
    call draw_text

    # Button 1: [ Browse ISO... ]
    mov dword ptr [rsp + 32], 718
    mov dword ptr [rsp + 40], 152
    mov dword ptr [rsp + 48], COL_SLATE
    mov dword ptr [rsp + 56], COL_SLATE_HOVER
    mov dword ptr [rsp + 64], COL_TEXT_WHITE
    lea rax, [rip + str_btn_browse]
    mov qword ptr [rsp + 72], rax
    mov dword ptr [rsp + 80], 20
    mov r9d, 118
    mov r8d, 590
    mov edx, 1
    mov rcx, rdi
    call draw_button

    # 4. Card 2: Build Profile & Windows PC Port (24, 180, 736, 254)
    mov dword ptr [rsp + 40], COL_CARD_BORDER
    mov dword ptr [rsp + 32], 255
    mov r9d, 737
    mov r8d, 179
    mov edx, 23
    mov rcx, rdi
    call fill_box

    mov dword ptr [rsp + 40], COL_CARD_BG
    mov dword ptr [rsp + 32], 254
    mov r9d, 736
    mov r8d, 180
    mov edx, 24
    mov rcx, rdi
    call fill_box

    mov rax, qword ptr [rip + g_font_btn]
    mov qword ptr [rsp + 40], rax
    mov dword ptr [rsp + 32], COL_TEXT_WHITE
    lea r9, [rip + str_card2_title]
    mov r8d, 186
    mov edx, 40
    mov rcx, rdi
    call draw_text

    # Toggle 2: Release Mode checkbox (44, 212, 360, 242)
    mov eax, COL_SLATE
    cmp dword ptr [rip + g_opt_release], 0
    je .Lchk1_bg
    mov eax, COL_TEAL
.Lchk1_bg:
    mov dword ptr [rsp + 32], 360
    mov dword ptr [rsp + 40], 242
    mov dword ptr [rsp + 48], eax
    mov dword ptr [rsp + 56], COL_TEAL_HOVER
    mov dword ptr [rsp + 64], COL_TEXT_WHITE
    lea rax, [rip + str_opt_rel_on]
    cmp dword ptr [rip + g_opt_release], 0
    jne .Lchk1_str
    lea rax, [rip + str_opt_rel_off]
.Lchk1_str:
    mov qword ptr [rsp + 72], rax
    mov dword ptr [rsp + 80], 14
    mov r9d, 212
    mov r8d, 44
    mov edx, 2
    mov rcx, rdi
    call draw_button

    # Button 3: BUILD WINDOWS PC PORT (.EXE) (394, 212, 716, 242)
    mov dword ptr [rsp + 32], 716
    mov dword ptr [rsp + 40], 242
    mov dword ptr [rsp + 48], COL_SLATE
    mov dword ptr [rsp + 56], COL_SLATE_HOVER
    mov dword ptr [rsp + 64], COL_TEXT_WHITE
    lea rax, [rip + str_btn_build_win]
    mov qword ptr [rsp + 72], rax
    mov dword ptr [rsp + 80], 42
    mov r9d, 212
    mov r8d, 394
    mov edx, 3
    mov rcx, rdi
    call draw_button

    # 5. Card 3: Nintendo Switch Native Package Targets (24, 266, 736, 432)
    mov dword ptr [rsp + 40], COL_CARD_BORDER
    mov dword ptr [rsp + 32], 433
    mov r9d, 737
    mov r8d, 265
    mov edx, 23
    mov rcx, rdi
    call fill_box

    mov dword ptr [rsp + 40], COL_CARD_BG
    mov dword ptr [rsp + 32], 432
    mov r9d, 736
    mov r8d, 266
    mov edx, 24
    mov rcx, rdi
    call fill_box

    mov rax, qword ptr [rip + g_font_btn]
    mov qword ptr [rsp + 40], rax
    mov dword ptr [rsp + 32], COL_TEXT_WHITE
    lea r9, [rip + str_card3_title]
    mov r8d, 274
    mov edx, 40
    mov rcx, rdi
    call draw_text

    # Button 4: BUILD SWITCH (.NRO + .NSP) (44, 304, 366, 356)
    mov dword ptr [rsp + 32], 366
    mov dword ptr [rsp + 40], 356
    mov dword ptr [rsp + 48], COL_GOLD
    mov dword ptr [rsp + 56], COL_GOLD_HOVER
    mov dword ptr [rsp + 64], COL_TEXT_DARK
    lea rax, [rip + str_btn_build_both]
    mov qword ptr [rsp + 72], rax
    mov dword ptr [rsp + 80], 24
    mov r9d, 304
    mov r8d, 44
    mov edx, 4
    mov rcx, rdi
    call draw_button

    # Button 5: BUILD SWITCH .NSP ONLY (394, 304, 716, 356)
    mov dword ptr [rsp + 32], 716
    mov dword ptr [rsp + 40], 356
    mov dword ptr [rsp + 48], COL_CYAN
    mov dword ptr [rsp + 56], COL_CYAN_HOVER
    mov dword ptr [rsp + 64], COL_TEXT_DARK
    lea rax, [rip + str_btn_build_nsp]
    mov qword ptr [rsp + 72], rax
    mov dword ptr [rsp + 80], 36
    mov r9d, 304
    mov r8d, 394
    mov edx, 5
    mov rcx, rdi
    call draw_button

    # Button 6: BUILD SWITCH .NRO ONLY (44, 370, 366, 416)
    mov dword ptr [rsp + 32], 366
    mov dword ptr [rsp + 40], 416
    mov dword ptr [rsp + 48], COL_TEAL
    mov dword ptr [rsp + 56], COL_TEAL_HOVER
    mov dword ptr [rsp + 64], COL_TEXT_WHITE
    lea rax, [rip + str_btn_build_nro]
    mov qword ptr [rsp + 72], rax
    mov dword ptr [rsp + 80], 38
    mov r9d, 370
    mov r8d, 44
    mov edx, 6
    mov rcx, rdi
    call draw_button

    # Button 7: OPEN OUTPUT FOLDER (394, 370, 716, 416)
    mov dword ptr [rsp + 32], 716
    mov dword ptr [rsp + 40], 416
    mov dword ptr [rsp + 48], COL_SLATE
    mov dword ptr [rsp + 56], COL_SLATE_HOVER
    mov dword ptr [rsp + 64], COL_TEXT_WHITE
    lea rax, [rip + str_btn_open_dir]
    mov qword ptr [rsp + 72], rax
    mov dword ptr [rsp + 80], 44
    mov r9d, 370
    mov r8d, 394
    mov edx, 7
    mov rcx, rdi
    call draw_button

    # 6. Status & Progress Footer (24, 444, 736, 496)
    mov dword ptr [rsp + 40], COL_INPUT_BG
    mov dword ptr [rsp + 32], 496
    mov r9d, 736
    mov r8d, 444
    mov edx, 24
    mov rcx, rdi
    call fill_box

    # Progress bar track (36, 478, 724, 488)
    mov dword ptr [rsp + 40], COL_CARD_BG
    mov dword ptr [rsp + 32], 488
    mov r9d, 724
    mov r8d, 478
    mov edx, 36
    mov rcx, rdi
    call fill_box

    # Progress bar fill
    mov eax, dword ptr [rip + g_progress_pct]
    imul eax, 688
    mov ecx, 100
    cdq
    idiv ecx
    add eax, 36
    mov dword ptr [rsp + 40], COL_GREEN
    mov dword ptr [rsp + 32], 488
    mov r9d, eax
    mov r8d, 478
    mov edx, 36
    mov rcx, rdi
    call fill_box

    # Status message
    mov rax, qword ptr [rip + g_font_body]
    mov qword ptr [rsp + 40], rax
    mov dword ptr [rsp + 32], COL_TEXT_WHITE
    lea r9, [rip + g_status_text]
    mov r8d, 452
    mov edx, 38
    mov rcx, rdi
    call draw_text

    lea rdx, [rsp + 64]
    mov rcx, rbx
    call EndPaint
    xor eax, eax
    jmp .Lwnd_ret

.Lwm_destroy:
    xor ecx, ecx
    call PostQuitMessage
    xor eax, eax

.Lwnd_ret:
    add rsp, 144
    pop rdi
    pop rsi
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Entry Point: WinMain / main
# ==============================================================================
.globl WinMain
.globl main
WinMain:
main:
    push rbp
    mov rbp, rsp
    push rbx
    push rdi
    sub rsp, 192

    xor ecx, ecx
    call GetModuleHandleA
    mov rbx, rax

    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_status_ready]
    call my_strcpy

    lea rdi, [rsp + 32]
    xor eax, eax
    mov ecx, 80
    rep stosb

    xor ecx, ecx
    mov edx, IDC_ARROW
    call LoadCursorA

    mov dword ptr [rsp + 32], 80
    mov dword ptr [rsp + 36], (CS_HREDRAW | CS_VREDRAW)
    lea rcx, [rip + WndProc]
    mov qword ptr [rsp + 40], rcx
    mov qword ptr [rsp + 56], rbx
    mov qword ptr [rsp + 72], rax
    mov qword ptr [rsp + 80], 0
    lea rcx, [rip + str_class_name]
    mov qword ptr [rsp + 96], rcx

    lea rcx, [rsp + 32]
    call RegisterClassExA

    mov qword ptr [rsp + 88], 0
    mov qword ptr [rsp + 80], rbx
    mov qword ptr [rsp + 72], 0
    mov qword ptr [rsp + 64], 0
    mov dword ptr [rsp + 56], 548
    mov dword ptr [rsp + 48], 776
    mov dword ptr [rsp + 40], CW_USEDEFAULT
    mov dword ptr [rsp + 32], CW_USEDEFAULT
    mov r9d, WS_STYLE_FIXED
    lea r8, [rip + str_win_title]
    lea rdx, [rip + str_class_name]
    xor ecx, ecx
    call CreateWindowExA
    mov rbx, rax

    mov edx, SW_SHOWNORMAL
    mov rcx, rbx
    call ShowWindow

    mov rcx, rbx
    call UpdateWindow

.Lmsg_loop:
    lea rcx, [rsp + 112]
    xor edx, edx
    xor r8d, r8d
    xor r9d, r9d
    call GetMessageA
    cmp eax, 0
    jle .Lmsg_exit

    lea rcx, [rsp + 112]
    call TranslateMessage
    lea rcx, [rsp + 112]
    call DispatchMessageA
    jmp .Lmsg_loop

.Lmsg_exit:
    xor ecx, ecx
    call ExitProcess

# ==============================================================================
# Read-Only Strings & Mutable State
# ==============================================================================
.data
    .align 16
zero_pad_16:    .space 16, 0

.include "tools/gui_builder/romfs_tables.inc"

    .align 8
g_hwnd:         .quad 0
g_font_title:   .quad 0
g_font_btn:     .quad 0
g_font_body:    .quad 0
g_iso_handle:   .quad -1
g_out_handle:   .quad -1
g_aes_ctr:      .quad 0
g_hover_btn:    .long 0
g_opt_release:  .long 1
g_progress_pct: .long 15
g_out_pos:      .long 0
g_is_nsp:       .long 0

str_class_name:
    .asciz "TSSMAsmBuilderWnd"
str_win_title:
    .asciz "The SpongeBob SquarePants Movie - Native Port Builder (x86_64 Assembly GUI)"
str_font_segoe:
    .asciz "Segoe UI"
str_header_title:
    .asciz "THE SPONGEBOB SQUAREPANTS MOVIE  -  NATIVE PORT BUILDER"
str_header_sub:
    .asciz "Self-Contained x86_64 Assembly + AES-NI Builder  |  GameCube ISO -> Switch .NRO, .NSP & PC .EXE"
str_card1_title:
    .asciz "1. GAMECUBE DISC IMAGE (.ISO / .GCM)"
str_card2_title:
    .asciz "2. COMPILER PROFILE & WINDOWS PC PORT"
str_card3_title:
    .asciz "3. NINTENDO SWITCH NATIVE PACKAGE TARGETS (NO DOCKER / DEVKITPRO REQUIRED)"
str_iso_placeholder:
    .asciz "No ISO selected yet (click Browse ISO... or place any .iso/.gcm in this folder)"
str_glob_iso:
    .asciz "*.iso"
str_glob_gcm:
    .asciz "*.gcm"
str_btn_browse:
    .asciz "Browse ISO..."
str_opt_rel_on:
    .asciz "[X] Release Profile (60 FPS Native LTO)"
str_opt_rel_off:
    .asciz "[  ] Dev Profile (Fast Debug Compile)"
str_btn_build_win:
    .asciz "BUILD WINDOWS PC PORT (.EXE)"
str_btn_build_both:
    .asciz "BUILD SWITCH (.NRO + .NSP)"
str_btn_build_nsp:
    .asciz "BUILD SWITCH .NSP ONLY (738 MB)"
str_btn_build_nro:
    .asciz "BUILD SWITCH .NRO ONLY (747 MB)"
str_btn_open_dir:
    .asciz "OPEN OUTPUT FOLDER (target\\switch)"

str_status_ready:
    .asciz "STATUS: Ready  |  Select your GameCube ISO and click any Switch or PC build target."
str_status_iso_set:
    .asciz "STATUS: Disc image selected -> "
str_status_building_win:
    .asciz "STATUS: Launching Windows PC port build pipeline (tools\\build.ps1)..."
str_status_building_nro:
    .asciz "STATUS: Building standalone Homebrew NRO (target\\switch\\tssm.nro) in x86_64 assembly..."
str_status_building_nsp:
    .asciz "STATUS: Building installable Switch NSP (target\\switch\\nsp\\0100780012340000.nsp) via AES-NI..."
str_status_nro_ok:
    .asciz "SUCCESS: Built target\\switch\\tssm.nro (747 MB All-in-One Homebrew NRO Ready!)"
str_status_nsp_ok:
    .asciz "SUCCESS: Built target\\switch\\nsp\\0100780012340000.nsp (738 MB Home Menu NSP Ready!)"
str_status_both_ok:
    .asciz "SUCCESS: Built BOTH target\\switch\\tssm.nro AND target\\switch\\nsp\\0100780012340000.nsp!"
str_status_opened_dir:
    .asciz "STATUS: Opened target\\switch output folder in Windows Explorer."

str_err_no_iso:
    .asciz "ERROR: No GameCube ISO selected! Please choose your .iso / .gcm file."
str_err_open_iso:
    .asciz "ERROR: Could not open the selected GameCube ISO file!"
str_err_bad_iso:
    .asciz "ERROR: Invalid disc image! Please select a valid GameCube ISO (.iso / .gcm)."
str_err_tpl_nro:
    .asciz "ERROR: Missing tools\\switch\\tssm_engine.nro template in repository!"
str_err_tpl_nsp:
    .asciz "ERROR: Missing tools\\switch\\tssm_nsp_template.bin template in repository!"
str_err_write:
    .asciz "ERROR: Disk write failed while generating Switch package!"

str_dir_target:
    .asciz "target"
str_dir_switch:
    .asciz "target\\switch"
str_dir_nsp:
    .asciz "target\\switch\\nsp"
str_path_engine_nro:
    .asciz "tools\\switch\\tssm_engine.nro"
str_path_nsp_tpl:
    .asciz "tools\\switch\\tssm_nsp_template.bin"
str_path_out_nro:
    .asciz "target\\switch\\tssm.nro"
str_path_out_nsp:
    .asciz "target\\switch\\nsp\\0100780012340000.nsp"

str_ofn_filter:
    .ascii "GameCube Disc Images (*.iso;*.gcm)\0*.iso;*.gcm\0All Files (*.*)\0*.*\0\0"
str_ofn_title:
    .asciz "Select The SpongeBob SquarePants Movie GameCube ISO"

str_open:
    .asciz "open"
str_powershell:
    .asciz "powershell.exe"
str_ps_base:
    .asciz "-NoProfile -ExecutionPolicy Bypass -File \"tools\\build.ps1\""
str_ps_iso_prefix:
    .asciz " -Iso \""
str_quote:
    .asciz "\""
str_ps_dev:
    .asciz " -Dev"

.bss
    .align 16
g_iso_path:     .space 520
g_status_text:  .space 600
g_cmd_args:     .space 1024
g_find_data:    .space 384
g_fst_buf:      .space 8192
g_io_buf:       .space 65536
g_out_buf:      .space 65536
