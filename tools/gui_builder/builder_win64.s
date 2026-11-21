# ==============================================================================
# The SpongeBob SquarePants Movie - Native GUI Builder (Windows x86_64 Assembly)
# ==============================================================================
# Pure x86_64 assembly (GAS Intel syntax) Win32 GDI application.
# Assembled & linked with:
#   x86_64-w64-mingw32-gcc -mwindows -O2 tools/gui_builder/builder_win64.s \
#       -o TSSM-Builder.exe -luser32 -lgdi32 -lcomdlg32 -lshell32 -lkernel32
# ==============================================================================

.intel_syntax noprefix
.text

# Win32 Constants
.equ CS_HREDRAW,          0x0002
.equ CS_VREDRAW,          0x0001
.equ IDC_ARROW,           32512
.equ IDC_HAND,            32649
.equ WS_OVERLAPPED,       0x00000000
.equ WS_CAPTION,          0x00C00000
.equ WS_SYSMENU,          0x00080000
.equ WS_MINIMIZEBOX,      0x00020000
.equ WS_VISIBLE,          0x10000000
.equ WS_STYLE_FIXED,      (WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE)
.equ CW_USEDEFAULT,       0x80000000

.equ WM_CREATE,           0x0001
.equ WM_DESTROY,          0x0002
.equ WM_PAINT,            0x000F
.equ WM_CLOSE,            0x0010
.equ WM_ERASEBKGND,       0x0014
.equ WM_SETCURSOR,        0x0020
.equ WM_KEYDOWN,          0x0100
.equ WM_MOUSEMOVE,        0x0200
.equ WM_LBUTTONDOWN,      0x0201

.equ TRANSPARENT,         1
.equ FW_NORMAL,           400
.equ FW_SEMIBOLD,         600
.equ FW_BOLD,             700
.equ DEFAULT_CHARSET,     1
.equ CLEARTYPE_QUALITY,   5
.equ NULL_BRUSH,          5
.equ SW_SHOWNORMAL,       1
.equ OFN_FILEMUSTEXIST,   0x00001000
.equ OFN_PATHMUSTEXIST,   0x00000800
.equ OFN_NOCHANGEDIR,     0x00000008
.equ INVALID_FILE_ATTR,   0xFFFFFFFF

# Color palette (Win32 COLORREF: 0x00BBGGRR)
.equ COL_BG_DARK,         0x0022130C   # Deep Bikini Bottom Navy (#0C1322)
.equ COL_CARD_BG,         0x00362015   # Card Navy (#152036)
.equ COL_CARD_BORDER,     0x005A3824   # Subtle Card Border (#24385A)
.equ COL_HEADER_BG,       0x002D1A10   # Header Navy (#101A2D)
.equ COL_GOLD,            0x0015C8FF   # SpongeBob Gold (#FFC815)
.equ COL_GOLD_HOVER,      0x0045D8FF   # Bright Gold Hover (#FFD845)
.equ COL_CYAN,            0x00F0B800   # Switch Neon Cyan (#00B8F0)
.equ COL_CYAN_HOVER,      0x00FFCE28   # Bright Cyan Hover (#28CEFF)
.equ COL_TEAL,            0x0096B814   # Seafoam Teal (#14B896)
.equ COL_TEAL_HOVER,      0x00B0D428   # Bright Teal Hover (#28D4B0)
.equ COL_SLATE,           0x00523828   # Slate Button (#283852)
.equ COL_SLATE_HOVER,     0x006E4C36   # Slate Hover (#364C6E)
.equ COL_INPUT_BG,        0x001B0F09   # Dark Input Field (#090F1B)
.equ COL_TEXT_WHITE,      0x00FAFAFA   # Crisp White
.equ COL_TEXT_MUTED,      0x00C8A890   # Soft Ocean Gray-Blue (#90A8C8)
.equ COL_TEXT_DARK,       0x00141008   # Dark text on Gold button
.equ COL_GREEN,           0x0070E030   # Status Ready Green

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
.extern CreatePen
.extern SelectObject
.extern DeleteObject
.extern GetStockObject
.extern FillRect
.extern RoundRect
.extern SetBkMode
.extern SetTextColor
.extern TextOutA
.extern CreateFontA
.extern GetOpenFileNameA
.extern ShellExecuteA
.extern GetFileAttributesA
.extern SetCursor
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
    mov rdi, rcx
    mov rsi, rdx
    call my_strlen
    add rdi, rax
    mov rcx, rdi
    mov rdx, rsi
    call my_strcpy
    pop rsi
    pop rdi
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

    # Pick normal or hover color
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

    # Draw subtle 2px top accent highlight strip on hovered button
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
    lea r8d, [r12 + rax - 9]      # vertically centered y
    mov edx, edi
    add edx, dword ptr [rbp + 96] # left + text_offset_x
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

    # Toggle 3: Force Rebuild (390, 210, 710, 240)
    mov dword ptr [rsp + 40], 240
    mov dword ptr [rsp + 32], 710
    mov r9d, 210
    mov r8d, 390
    mov edx, esi
    mov ecx, ebx
    call point_in_rect
    test eax, eax
    jnz .Lht_3

    # Button 4: Build Windows PC Port (.exe) (44, 304, 366, 356)
    mov dword ptr [rsp + 40], 356
    mov dword ptr [rsp + 32], 366
    mov r9d, 304
    mov r8d, 44
    mov edx, esi
    mov ecx, ebx
    call point_in_rect
    test eax, eax
    jnz .Lht_4

    # Button 5: Build Nintendo Switch Port (.nro / .nsp) (394, 304, 716, 356)
    mov dword ptr [rsp + 40], 356
    mov dword ptr [rsp + 32], 716
    mov r9d, 304
    mov r8d, 394
    mov edx, esi
    mov ecx, ebx
    call point_in_rect
    test eax, eax
    jnz .Lht_5

    # Button 6: Extract Disc Assets Only (44, 370, 366, 416)
    mov dword ptr [rsp + 40], 416
    mov dword ptr [rsp + 32], 366
    mov r9d, 370
    mov r8d, 44
    mov edx, esi
    mov ecx, ebx
    call point_in_rect
    test eax, eax
    jnz .Lht_6

    # Button 7: Open Project Folder (394, 370, 716, 416)
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
    sub rsp, 192

    # Zero OPENFILENAMEA struct (152 bytes on x64)
    lea rdi, [rsp + 32]
    xor eax, eax
    mov ecx, 152
    rep stosb

    mov dword ptr [rsp + 32], 152                 # lStructSize = sizeof(OPENFILENAMEA)
    mov qword ptr [rsp + 40], rcx                 # hwndOwner
    lea rax, [rip + str_ofn_filter]
    mov qword ptr [rsp + 56], rax                 # lpstrFilter
    lea rax, [rip + g_iso_path]
    mov qword ptr [rsp + 80], rax                 # lpstrFile
    mov dword ptr [rsp + 88], 512                 # nMaxFile
    lea rax, [rip + str_ofn_title]
    mov qword ptr [rsp + 112], rax                # lpstrTitle
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

    # Build PowerShell arguments: -NoProfile -ExecutionPolicy Bypass -File "tools\build.ps1"
    lea rcx, [rip + g_cmd_args]
    lea rdx, [rip + str_ps_base]
    call my_strcpy

    # Append -Iso "<g_iso_path>" if non-empty
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
    jne .Lwin_chk_force
    lea rcx, [rip + g_cmd_args]
    lea rdx, [rip + str_ps_dev]
    call my_strcat

.Lwin_chk_force:
    cmp dword ptr [rip + g_opt_force], 0
    je .Lwin_exec
    lea rcx, [rip + g_cmd_args]
    lea rdx, [rip + str_ps_force]
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
    # Create UI fonts (Segoe UI)
    lea rax, [rip + str_font_segoe]
    mov qword ptr [rsp + 96], rax
    mov dword ptr [rsp + 88], 0
    mov dword ptr [rsp + 80], CLEARTYPE_QUALITY
    mov dword ptr [rsp + 72], 0
    mov dword ptr [rsp + 64], 0
    mov dword ptr [rsp + 56], DEFAULT_CHARSET
    mov dword ptr [rsp + 48], 0
    mov dword ptr [rsp + 40], 0
    mov dword ptr [rsp + 32], 0
    mov r9d, 0
    mov r8d, 0
    mov edx, 0
    mov ecx, -21                  # Title 21px
    mov dword ptr [rsp + 32], FW_BOLD
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
    mov ecx, -15                  # Button / Card header 15px
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
    mov ecx, -14                  # Body 14px
    call CreateFontA
    mov qword ptr [rip + g_font_body], rax

    # Check if default ISO exists in current folder
    lea rcx, [rip + str_default_iso]
    call GetFileAttributesA
    cmp eax, INVALID_FILE_ATTR
    je .Lwm_create_done
    lea rcx, [rip + g_iso_path]
    lea rdx, [rip + str_default_iso]
    call my_strcpy
    mov dword ptr [rip + g_progress_pct], 25
.Lwm_create_done:
    xor eax, eax
    jmp .Lwnd_ret

.Lwm_mousemove:
    movzx ecx, r9w                # LOWORD(lParam) = mx
    shr r9, 16
    movzx edx, r9w                # HIWORD(lParam) = my
    call hit_test_buttons
    cmp eax, dword ptr [rip + g_hover_btn]
    je .Lmm_no_change
    mov dword ptr [rip + g_hover_btn], eax
    mov r8d, 0
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
    je .Lclick_toggle_force
    cmp eax, 4
    je .Lclick_build_win
    cmp eax, 5
    je .Lclick_build_switch
    cmp eax, 6
    je .Lclick_extract
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

.Lclick_toggle_force:
    xor dword ptr [rip + g_opt_force], 1
    jmp .Lclick_refresh

.Lclick_build_win:
    mov rcx, rbx
    call run_windows_build
    jmp .Lclick_refresh

.Lclick_build_switch:
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_status_building_sw]
    call my_strcpy
    mov dword ptr [rip + g_progress_pct], 100
    mov dword ptr [rsp + 40], SW_SHOWNORMAL
    mov qword ptr [rsp + 32], 0
    lea r9, [rip + str_switch_args]
    lea r8, [rip + str_bash]
    lea rdx, [rip + str_open]
    mov rcx, rbx
    call ShellExecuteA
    jmp .Lclick_refresh

.Lclick_extract:
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_status_extract]
    call my_strcpy
    mov dword ptr [rip + g_progress_pct], 60
    mov dword ptr [rsp + 40], SW_SHOWNORMAL
    mov qword ptr [rsp + 32], 0
    lea r9, [rip + str_extract_args]
    lea r8, [rip + str_cmd]
    lea rdx, [rip + str_open]
    mov rcx, rbx
    call ShellExecuteA
    jmp .Lclick_refresh

.Lclick_open_dir:
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_status_opened_dir]
    call my_strcpy
    mov dword ptr [rsp + 40], SW_SHOWNORMAL
    mov qword ptr [rsp + 32], 0
    xor r9d, r9d
    lea r8, [rip + str_dot]
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
    lea rdx, [rsp + 64]           # PAINTSTRUCT (72 bytes)
    mov rcx, rbx
    call BeginPaint
    mov rdi, rax                  # hdc

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

    # Header Title & Subtitle
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
    mov dword ptr [rsp + 64], 20
    lea rax, [rip + str_btn_browse]
    mov qword ptr [rsp + 56], rax
    mov dword ptr [rsp + 48], COL_TEXT_WHITE
    mov dword ptr [rsp + 40], COL_SLATE_HOVER
    mov dword ptr [rsp + 32], COL_SLATE
    mov dword ptr [rsp + 24], 152
    mov dword ptr [rsp + 16], 718
    # use draw_button stack layout:
    mov dword ptr [rsp + 56], 20
    lea rax, [rip + str_btn_browse]
    mov qword ptr [rsp + 48], rax
    mov dword ptr [rsp + 40], COL_TEXT_WHITE
    mov dword ptr [rsp + 32], COL_SLATE_HOVER
    # wait: let's pass draw_button args at [rsp+32..rsp+88]:
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

    # 4. Card 2: Build Profile & Recompiler Options (24, 180, 736, 254)
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

    # Toggle 3: Force Rebuild checkbox (394, 212, 716, 242)
    mov eax, COL_SLATE
    cmp dword ptr [rip + g_opt_force], 0
    je .Lchk2_bg
    mov eax, COL_TEAL
.Lchk2_bg:
    mov dword ptr [rsp + 32], 716
    mov dword ptr [rsp + 40], 242
    mov dword ptr [rsp + 48], eax
    mov dword ptr [rsp + 56], COL_TEAL_HOVER
    mov dword ptr [rsp + 64], COL_TEXT_WHITE
    lea rax, [rip + str_opt_force_on]
    cmp dword ptr [rip + g_opt_force], 0
    jne .Lchk2_str
    lea rax, [rip + str_opt_force_off]
.Lchk2_str:
    mov qword ptr [rsp + 72], rax
    mov dword ptr [rsp + 80], 14
    mov r9d, 212
    mov r8d, 394
    mov edx, 3
    mov rcx, rdi
    call draw_button

    # 5. Card 3: Build & Package Actions (24, 266, 736, 432)
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

    # Button 4: BUILD WINDOWS PC PORT (.EXE) (44, 304, 366, 356)
    mov dword ptr [rsp + 32], 366
    mov dword ptr [rsp + 40], 356
    mov dword ptr [rsp + 48], COL_GOLD
    mov dword ptr [rsp + 56], COL_GOLD_HOVER
    mov dword ptr [rsp + 64], COL_TEXT_DARK
    lea rax, [rip + str_btn_build_win]
    mov qword ptr [rsp + 72], rax
    mov dword ptr [rsp + 80], 34
    mov r9d, 304
    mov r8d, 44
    mov edx, 4
    mov rcx, rdi
    call draw_button

    # Button 5: BUILD NINTENDO SWITCH (.NRO + .NSP) (394, 304, 716, 356)
    mov dword ptr [rsp + 32], 716
    mov dword ptr [rsp + 40], 356
    mov dword ptr [rsp + 48], COL_CYAN
    mov dword ptr [rsp + 56], COL_CYAN_HOVER
    mov dword ptr [rsp + 64], COL_TEXT_DARK
    lea rax, [rip + str_btn_build_sw]
    mov qword ptr [rsp + 72], rax
    mov dword ptr [rsp + 80], 24
    mov r9d, 304
    mov r8d, 394
    mov edx, 5
    mov rcx, rdi
    call draw_button

    # Button 6: EXTRACT DISC ASSETS ONLY (44, 370, 366, 416)
    mov dword ptr [rsp + 32], 366
    mov dword ptr [rsp + 40], 416
    mov dword ptr [rsp + 48], COL_SLATE
    mov dword ptr [rsp + 56], COL_SLATE_HOVER
    mov dword ptr [rsp + 64], COL_TEXT_WHITE
    lea rax, [rip + str_btn_extract]
    mov qword ptr [rsp + 72], rax
    mov dword ptr [rsp + 80], 54
    mov r9d, 370
    mov r8d, 44
    mov edx, 6
    mov rcx, rdi
    call draw_button

    # Button 7: OPEN PROJECT FOLDER (394, 370, 716, 416)
    mov dword ptr [rsp + 32], 716
    mov dword ptr [rsp + 40], 416
    mov dword ptr [rsp + 48], COL_SLATE
    mov dword ptr [rsp + 56], COL_SLATE_HOVER
    mov dword ptr [rsp + 64], COL_TEXT_WHITE
    lea rax, [rip + str_btn_open_dir]
    mov qword ptr [rsp + 72], rax
    mov dword ptr [rsp + 80], 68
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
    sub rsp, 184

    xor ecx, ecx
    call GetModuleHandleA
    mov rbx, rax                  # hInstance

    # Initialize status string
    lea rcx, [rip + g_status_text]
    lea rdx, [rip + str_status_ready]
    call my_strcpy

    # Zero WNDCLASSEXA (80 bytes) at [rsp + 32]
    lea rdi, [rsp + 32]
    xor eax, eax
    mov ecx, 80
    rep stosb

    xor ecx, ecx
    mov edx, IDC_ARROW
    call LoadCursorA

    mov dword ptr [rsp + 32], 80                  # cbSize = 80
    mov dword ptr [rsp + 36], (CS_HREDRAW | CS_VREDRAW)
    lea rcx, [rip + WndProc]
    mov qword ptr [rsp + 40], rcx                 # lpfnWndProc
    mov qword ptr [rsp + 56], rbx                 # hInstance
    mov qword ptr [rsp + 72], rax                 # hCursor
    mov qword ptr [rsp + 80], 0                   # hbrBackground
    lea rcx, [rip + str_class_name]
    mov qword ptr [rsp + 96], rcx                 # lpszClassName

    lea rcx, [rsp + 32]
    call RegisterClassExA

    # CreateWindowExA
    mov qword ptr [rsp + 88], 0                   # lpParam
    mov qword ptr [rsp + 80], rbx                 # hInstance
    mov qword ptr [rsp + 72], 0                   # hMenu
    mov qword ptr [rsp + 64], 0                   # hWndParent
    mov dword ptr [rsp + 56], 548                 # nHeight
    mov dword ptr [rsp + 48], 776                 # nWidth
    mov dword ptr [rsp + 40], CW_USEDEFAULT       # Y
    mov dword ptr [rsp + 32], CW_USEDEFAULT       # X
    mov r9d, WS_STYLE_FIXED                       # dwStyle
    lea r8, [rip + str_win_title]                 # lpWindowName
    lea rdx, [rip + str_class_name]               # lpClassName
    xor ecx, ecx                                  # dwExStyle
    call CreateWindowExA
    mov rbx, rax

    mov edx, SW_SHOWNORMAL
    mov rcx, rbx
    call ShowWindow

    mov rcx, rbx
    call UpdateWindow

.Lmsg_loop:
    lea rcx, [rsp + 112]                          # MSG struct (48 bytes)
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
    .align 8
g_font_title:   .quad 0
g_font_btn:     .quad 0
g_font_body:    .quad 0
g_hover_btn:    .long 0
g_opt_release:  .long 1
g_opt_force:    .long 0
g_progress_pct: .long 15

str_class_name:
    .asciz "TSSMAsmBuilderWnd"
str_win_title:
    .asciz "The SpongeBob SquarePants Movie - Native Port Builder (x86_64 Assembly GUI)"
str_font_segoe:
    .asciz "Segoe UI"
str_header_title:
    .asciz "THE SPONGEBOB SQUAREPANTS MOVIE  -  NATIVE PORT BUILDER"
str_header_sub:
    .asciz "Pure x86_64 Assembly GUI  |  GameCube USA (GGVE78 Rev 1)  ->  Windows PC & Nintendo Switch"
str_card1_title:
    .asciz "1. GAMECUBE DISC IMAGE (.ISO / .GCM)"
str_card2_title:
    .asciz "2. RECOMPILER & COMPILER PROFILE"
str_card3_title:
    .asciz "3. BUILD & PACKAGE TARGETS"
str_iso_placeholder:
    .asciz "No ISO selected yet (click Browse ISO... or build.ps1 will prompt)"
str_default_iso:
    .asciz "Nickelodeon SpongeBob SquarePants - The Movie (United Kingdom).iso"
str_btn_browse:
    .asciz "Browse ISO..."
str_opt_rel_on:
    .asciz "[X] Release Profile (60 FPS Native LTO)"
str_opt_rel_off:
    .asciz "[  ] Dev Profile (Fast Debug Compile)"
str_opt_force_on:
    .asciz "[X] Force Full Disc Extract & Recompile"
str_opt_force_off:
    .asciz "[  ] Incremental Build (Skip If Up-To-Date)"
str_btn_build_win:
    .asciz "BUILD WINDOWS PC PORT (.EXE)"
str_btn_build_sw:
    .asciz "BUILD NINTENDO SWITCH (.NRO/.NSP)"
str_btn_extract:
    .asciz "EXTRACT DISC ASSETS ONLY"
str_btn_open_dir:
    .asciz "OPEN PROJECT FOLDER"

str_status_ready:
    .asciz "STATUS: Ready  |  Select your GameCube GGVE78 (Rev 1) ISO and click a build target."
str_status_iso_set:
    .asciz "STATUS: Disc image selected -> "
str_status_building_win:
    .asciz "STATUS: Launching Windows PC port build pipeline (tools\\build.ps1)..."
str_status_building_sw:
    .asciz "STATUS: Launching Nintendo Switch build pipeline (./build_switch.sh)..."
str_status_extract:
    .asciz "STATUS: Extracting GameCube disc filesystem into disc/..."
str_status_opened_dir:
    .asciz "STATUS: Opened project workspace folder in Windows Explorer."

str_ofn_filter:
    .ascii "GameCube Disc Images (*.iso;*.gcm)\0*.iso;*.gcm\0All Files (*.*)\0*.*\0\0"
str_ofn_title:
    .asciz "Select The SpongeBob SquarePants Movie GameCube ISO (GGVE78 Rev 1)"

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
str_ps_force:
    .asciz " -Force"
str_bash:
    .asciz "bash.exe"
str_switch_args:
    .asciz "-c \"./build_switch.sh; read -p 'Press Enter to close...'\""
str_cmd:
    .asciz "cmd.exe"
str_extract_args:
    .asciz "/c cargo run --release -p recomp --bin extract & pause"
str_dot:
    .asciz "."

.bss
    .align 16
g_iso_path:     .space 520
g_status_text:  .space 600
g_cmd_args:     .space 1024
