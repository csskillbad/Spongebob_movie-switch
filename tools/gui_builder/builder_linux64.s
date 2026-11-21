# ==============================================================================
# The SpongeBob SquarePants Movie - Native GUI Builder (Linux x86_64 Assembly)
# ==============================================================================
# Pure x86_64 assembly (GAS Intel syntax, System V AMD64 ABI) X11 GUI builder.
# Assembled & linked with:
#   gcc -no-pie -O2 -s tools/gui_builder/builder_linux64.s \
#       /usr/lib64/libX11.so.6 -o tssm-builder
# ==============================================================================

.intel_syntax noprefix
.text

# X11 Constants
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
.extern XDrawRectangle
.extern XDrawString
.extern XFlush
.extern XCloseDisplay
.extern strlen
.extern strcpy
.extern strcat
.extern strcmp
.extern access
.extern popen
.extern fgets
.extern pclose
.extern fork
.extern execl
.extern puts
.extern _exit

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
    # Button 5: Package Switch NSP Only (394, 304, 322, 52) -> x:394..716, y:304..356
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
    # Button 6: Extract GameCube ISO (44, 370, 322, 46) -> x:44..366, y:370..416
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
# Forks and runs `/bin/sh -c <cmd_str>` asynchronously in background
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

    # Child process: execl("/bin/sh", "sh", "-c", cmd_str, NULL)
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
# Helper: browse_iso_zenity()
# Opens Zenity file picker via popen() and updates g_iso_path
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

    # Strip trailing newline
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

    # Button 5: PACKAGE SWITCH NSP ONLY (394, 304, 322, 52)
    sub rsp, 48
    mov dword ptr [rsp + 24], 42
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

    # Button 6: EXTRACT GAMECUBE ISO ASSETS (44, 370, 322, 46)
    sub rsp, 48
    mov dword ptr [rsp + 24], 48
    lea rax, [rip + str_btn_extract]
    mov qword ptr [rsp + 16], rax
    mov dword ptr [rsp + 8], COL_TEXT_WHITE
    mov dword ptr [rsp], COL_SLATE_HOVER
    mov r9d, COL_SLATE
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
    sub rsp, 208                  # 192 bytes for XEvent + locals

    # Check if `--check` or `--help` was passed
    cmp edi, 1
    jle .Linit_state
    mov rbx, qword ptr [rsi + 8]
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

.Linit_state:
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_status_ready]
    call strcpy

    # Auto-detect ISO in current folder
    lea rdi, [rip + str_default_iso]
    xor esi, esi                  # F_OK = 0
    call access
    test eax, eax
    jnz .Lopen_x11
    lea rdi, [rip + g_iso_path]
    lea rsi, [rip + str_default_iso]
    call strcpy
    mov dword ptr [rip + g_progress_pct], 25

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
    mov rbx, rax                  # root window

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

    # Set Window Title
    mov rdi, qword ptr [rip + g_dpy]
    mov rsi, rax
    lea rdx, [rip + str_win_title]
    call XStoreName

    # Register WM_DELETE_WINDOW protocol for clean close
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

    # Select Input Events
    mov rdi, qword ptr [rip + g_dpy]
    mov rsi, qword ptr [rip + g_win]
    mov edx, EVENT_MASK
    call XSelectInput

    # Map Window
    mov rdi, qword ptr [rip + g_dpy]
    mov rsi, qword ptr [rip + g_win]
    call XMapWindow

    call render_gui

.Levent_loop:
    mov rdi, qword ptr [rip + g_dpy]
    lea rsi, [rbp - 208]          # XEvent buffer
    call XNextEvent

    mov eax, dword ptr [rbp - 208]# event.type
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
    # XMotionEvent: x is at offset +64, y is at offset +68
    mov edi, dword ptr [rbp - 208 + 64]
    mov esi, dword ptr [rbp - 208 + 68]
    call hit_test
    cmp eax, dword ptr [rip + g_hover_btn]
    je .Levent_loop
    mov dword ptr [rip + g_hover_btn], eax
    call render_gui
    jmp .Levent_loop

.Lev_button:
    # XButtonEvent: x is at offset +64, y is at offset +68
    mov edi, dword ptr [rbp - 208 + 64]
    mov esi, dword ptr [rbp - 208 + 68]
    call hit_test
    cmp eax, 1
    je .Lact_browse
    cmp eax, 2
    je .Lact_toggle_rel
    cmp eax, 3
    je .Lact_toggle_nsp
    cmp eax, 4
    je .Lact_build_sw
    cmp eax, 5
    je .Lact_pkg_nsp
    cmp eax, 6
    je .Lact_extract
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

.Lact_build_sw:
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_status_build_sw]
    call strcpy
    mov dword ptr [rip + g_progress_pct], 100
    call render_gui
    lea rdi, [rip + str_cmd_build_sw]
    call spawn_shell_cmd
    jmp .Levent_loop

.Lact_pkg_nsp:
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_status_pkg_nsp]
    call strcpy
    mov dword ptr [rip + g_progress_pct], 100
    call render_gui
    lea rdi, [rip + str_cmd_pkg_nsp]
    call spawn_shell_cmd
    jmp .Levent_loop

.Lact_extract:
    lea rdi, [rip + g_status_text]
    lea rsi, [rip + str_status_extract]
    call strcpy
    mov dword ptr [rip + g_progress_pct], 60
    call render_gui
    lea rdi, [rip + str_cmd_extract]
    call spawn_shell_cmd
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
    add rsp, 208
    pop r12
    pop rbx
    pop rbp
    ret

# ==============================================================================
# Data Section
# ==============================================================================
.data
    .align 8
g_dpy:          .quad 0
g_win:          .quad 0
g_gc:           .quad 0
g_wm_delete:    .quad 0
g_screen:       .long 0
g_hover_btn:    .long 0
g_opt_release:  .long 1
g_opt_nsp:      .long 1
g_progress_pct: .long 15

str_arg_check:
    .asciz "--check"
str_arg_help:
    .asciz "--help"
str_cli_banner:
    .asciz "TSSM Native Port Builder (x86_64 Assembly X11 GUI) - Ready. Run without flags on X11/Wayland to open GUI."
str_err_display:
    .asciz "Error: Cannot open X11 display ($DISPLAY). Use ./build_switch.sh for headless terminal builds."
str_wm_delete:
    .asciz "WM_DELETE_WINDOW"
str_win_title:
    .asciz "The SpongeBob SquarePants Movie - Native Port Builder (x86_64 Assembly GUI)"

str_header_title:
    .asciz "THE SPONGEBOB SQUAREPANTS MOVIE  -  NATIVE PORT BUILDER"
str_header_sub:
    .asciz "Pure x86_64 Assembly X11 GUI  |  GameCube USA (GGVE78 Rev 1)  ->  Nintendo Switch & PC"
str_card1_title:
    .asciz "1. GAMECUBE DISC IMAGE (.ISO / .GCM)"
str_card2_title:
    .asciz "2. COMPILER & SWITCH PACKAGING PROFILE"
str_card3_title:
    .asciz "3. BUILD & PACKAGE TARGETS (CLICK TO LAUNCH)"
str_iso_placeholder:
    .asciz "No ISO selected (click Browse ISO... or place GGVE78 ISO in project root)"
str_default_iso:
    .asciz "Nickelodeon SpongeBob SquarePants - The Movie (United Kingdom).iso"
str_btn_browse:
    .asciz "Browse ISO..."
str_opt_rel_on:
    .asciz "[X] Release Profile (Cortex-A57 60 FPS LTO)"
str_opt_rel_off:
    .asciz "[ ] Dev Profile (Fast Debug Build)"
str_opt_nsp_on:
    .asciz "[X] Package Standalone NSP (0100780012340000)"
str_opt_nsp_off:
    .asciz "[ ] Homebrew NRO Only (Skip NSP Packaging)"
str_btn_build_sw:
    .asciz "BUILD NINTENDO SWITCH (.NRO + .NSP)"
str_btn_pkg_nsp:
    .asciz "PACKAGE SWITCH NSP ONLY"
str_btn_extract:
    .asciz "EXTRACT GAMECUBE ISO ASSETS"
str_btn_open_dir:
    .asciz "OPEN OUTPUT FOLDER (target/switch)"

str_status_ready:
    .asciz "STATUS: Ready  |  Click 'BUILD NINTENDO SWITCH' to compile NRO + NSP in background."
str_status_iso_set:
    .asciz "STATUS: Disc image selected -> "
str_status_build_sw:
    .asciz "STATUS: Building Nintendo Switch port (./build_switch.sh running in background -> build_gui.log)..."
str_status_pkg_nsp:
    .asciz "STATUS: Packaging standalone Switch NSP (tools/make_nsp.sh -> target/switch/nsp/0100780012340000.nsp)..."
str_status_extract:
    .asciz "STATUS: Extracting GameCube disc assets into disc/files/..."
str_status_open_dir:
    .asciz "STATUS: Opened target/switch output directory."

str_popen_r:
    .asciz "r"
str_zenity_cmd:
    .asciz "zenity --file-selection --title='Select GameCube ISO (GGVE78 Rev 1)' --file-filter='*.iso *.gcm' 2>/dev/null"
str_bin_sh:
    .asciz "/bin/sh"
str_sh_arg0:
    .asciz "sh"
str_sh_arg1:
    .asciz "-c"
str_cmd_build_sw:
    .asciz "./build_switch.sh > build_gui.log 2>&1"
str_cmd_pkg_nsp:
    .asciz "bash tools/make_nsp.sh >> build_gui.log 2>&1"
str_cmd_extract:
    .asciz "cargo run --release -p recomp --bin extract -- \"Nickelodeon SpongeBob SquarePants - The Movie (United Kingdom).iso\" disc > build_gui.log 2>&1"
str_cmd_open_dir:
    .asciz "mkdir -p target/switch && xdg-open target/switch >/dev/null 2>&1"

.bss
    .align 16
g_iso_path:     .space 520
g_status_text:  .space 600
