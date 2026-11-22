//! Native Win32 GUI for TSSM-Builder.exe (Windows 7 / 8 / 10 / 11 x64).
//! Uses only standard Windows system DLLs (kernel32, user32, gdi32, comdlg32, shell32, comctl32).
//! Supports double-click launch, drag-and-drop onto the .exe, drag-and-drop onto the window,
//! HiDPI scaling, non-blocking background worker thread, live progress bar, and MessageBox dialogs.

#![cfg(windows)]

use std::collections::VecDeque;
use std::ffi::{c_void, OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::{Mutex, OnceLock};
use std::thread;

use crate::builder::{
    auto_find_iso, auto_find_prod_keys, build_both, build_nro, build_nsp, ProjectPaths,
};
use crate::gcm::parse_iso;

type HWND = *mut c_void;
type HINSTANCE = *mut c_void;
type HICON = *mut c_void;
type HCURSOR = *mut c_void;
type HBRUSH = *mut c_void;
type HFONT = *mut c_void;
type HMENU = *mut c_void;
type HDC = *mut c_void;
type HDROP = *mut c_void;
type WPARAM = usize;
type LPARAM = isize;
type LRESULT = isize;
type BOOL = i32;

type WndProcFn = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

#[repr(C)]
struct WNDCLASSEXW {
    cb_size: u32,
    style: u32,
    lpfn_wnd_proc: WndProcFn,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: HINSTANCE,
    h_icon: HICON,
    h_cursor: HCURSOR,
    hbr_background: HBRUSH,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
    h_icon_sm: HICON,
}

#[repr(C)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
struct MSG {
    hwnd: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
    time: u32,
    pt: POINT,
}

#[repr(C)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct INITCOMMONCONTROLSEX {
    dw_size: u32,
    dw_icc: u32,
}

#[repr(C)]
struct OPENFILENAMEW {
    l_struct_size: u32,
    hwnd_owner: HWND,
    h_instance: HINSTANCE,
    lpstr_filter: *const u16,
    lpstr_custom_filter: *mut u16,
    n_max_cust_filter: u32,
    n_filter_index: u32,
    lpstr_file: *mut u16,
    n_max_file: u32,
    lpstr_file_title: *mut u16,
    n_max_file_title: u32,
    lpstr_initial_dir: *const u16,
    lpstr_title: *const u16,
    flags: u32,
    n_file_offset: u16,
    n_file_extension: u16,
    lpstr_def_ext: *const u16,
    l_cust_data: isize,
    lpfn_hook: usize,
    lp_template_name: *const u16,
    pv_reserved: *mut c_void,
    dw_reserved: u32,
    flags_ex: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(lpModuleName: *const u16) -> HINSTANCE;
    fn GetLastError() -> u32;
    fn AttachConsole(dwProcessId: u32) -> BOOL;
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(lpwcx: *const WNDCLASSEXW) -> u16;
    fn CreateWindowExW(
        dwExStyle: u32,
        lpClassName: *const u16,
        lpWindowName: *const u16,
        dwStyle: u32,
        x: i32,
        y: i32,
        nWidth: i32,
        nHeight: i32,
        hWndParent: HWND,
        hMenu: HMENU,
        hInstance: HINSTANCE,
        lpParam: *mut c_void,
    ) -> HWND;
    fn DefWindowProcW(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT;
    fn ShowWindow(hwnd: HWND, nCmdShow: i32) -> BOOL;
    fn UpdateWindow(hwnd: HWND) -> BOOL;
    fn GetMessageW(lpMsg: *mut MSG, hwnd: HWND, wMsgFilterMin: u32, wMsgFilterMax: u32) -> BOOL;
    fn TranslateMessage(lpMsg: *const MSG) -> BOOL;
    fn DispatchMessageW(lpMsg: *const MSG) -> LRESULT;
    fn PostQuitMessage(nExitCode: i32);
    fn PostMessageW(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> BOOL;
    fn SendMessageW(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT;
    fn SetWindowTextW(hwnd: HWND, lpString: *const u16) -> BOOL;
    fn GetWindowTextW(hwnd: HWND, lpString: *mut u16, nMaxCount: i32) -> i32;
    fn GetWindowTextLengthW(hwnd: HWND) -> i32;
    fn EnableWindow(hwnd: HWND, bEnable: BOOL) -> BOOL;
    fn MessageBoxW(hwnd: HWND, lpText: *const u16, lpCaption: *const u16, uType: u32) -> i32;
    fn LoadIconW(hInstance: HINSTANCE, lpIconName: *const u16) -> HICON;
    fn LoadCursorW(hInstance: HINSTANCE, lpCursorName: *const u16) -> HCURSOR;
    fn GetSystemMetrics(nIndex: i32) -> i32;
    fn GetDC(hwnd: HWND) -> HDC;
    fn ReleaseDC(hwnd: HWND, hdc: HDC) -> i32;
    fn DestroyWindow(hwnd: HWND) -> BOOL;
    fn SetForegroundWindow(hwnd: HWND) -> BOOL;
    fn AdjustWindowRectEx(lpRect: *mut RECT, dwStyle: u32, bMenu: BOOL, dwExStyle: u32) -> BOOL;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateFontW(
        cHeight: i32,
        cWidth: i32,
        cEscapement: i32,
        cOrientation: i32,
        cWeight: i32,
        bItalic: u32,
        bUnderline: u32,
        bStrikeOut: u32,
        iCharSet: u32,
        iOutPrecision: u32,
        iClipPrecision: u32,
        iQuality: u32,
        iPitchAndFamily: u32,
        pszFaceName: *const u16,
    ) -> HFONT;
    fn GetDeviceCaps(hdc: HDC, index: i32) -> i32;
}

#[link(name = "comdlg32")]
extern "system" {
    fn GetOpenFileNameW(lpofn: *mut OPENFILENAMEW) -> BOOL;
}

#[link(name = "shell32")]
extern "system" {
    fn DragAcceptFiles(hwnd: HWND, fAccept: BOOL);
    fn DragQueryFileW(hDrop: HDROP, iFile: u32, lpszFile: *mut u16, cch: u32) -> u32;
    fn DragFinish(hDrop: HDROP);
    fn ShellExecuteW(
        hwnd: HWND,
        lpOperation: *const u16,
        lpFile: *const u16,
        lpParameters: *const u16,
        lpDirectory: *const u16,
        nShowCmd: i32,
    ) -> HINSTANCE;
}

#[link(name = "comctl32")]
extern "system" {
    fn InitCommonControlsEx(picce: *const INITCOMMONCONTROLSEX) -> BOOL;
}

// Win32 Constants
const CS_HREDRAW: u32 = 0x0002;
const CS_VREDRAW: u32 = 0x0001;
const WS_OVERLAPPED: u32 = 0x0000_0000;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_MINIMIZEBOX: u32 = 0x0002_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_CHILD: u32 = 0x4000_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_EX_CLIENTEDGE: u32 = 0x0000_0200;
const WS_EX_ACCEPTFILES: u32 = 0x0000_0010;

const ES_AUTOHSCROLL: u32 = 0x0080;
const ES_MULTILINE: u32 = 0x0004;
const ES_AUTOVSCROLL: u32 = 0x0040;
const ES_READONLY: u32 = 0x0800;

const BS_PUSHBUTTON: u32 = 0x0000;
const BS_DEFPUSHBUTTON: u32 = 0x0001;
const SS_LEFT: u32 = 0x0000;
const SS_NOPREFIX: u32 = 0x0080;

const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_SETFONT: u32 = 0x0030;
const WM_COMMAND: u32 = 0x0111;
const WM_DROPFILES: u32 = 0x0233;
const WM_APP: u32 = 0x8000;
const WM_APP_EVENT: u32 = WM_APP + 1;
const WM_APP_AUTOBUILD: u32 = WM_APP + 2;

const EM_SETSEL: u32 = 0x00B1;
const EM_REPLACESEL: u32 = 0x00C2;
const EM_SETLIMITTEXT: u32 = 0x00C5;
const EM_SCROLLCARET: u32 = 0x00B7;

const _: () = {
    assert!(std::mem::size_of::<WNDCLASSEXW>() == 80);
    assert!(std::mem::size_of::<MSG>() == 48);
    assert!(std::mem::size_of::<RECT>() == 16);
    assert!(std::mem::size_of::<INITCOMMONCONTROLSEX>() == 8);
    assert!(std::mem::size_of::<OPENFILENAMEW>() == 152);
};

const PBM_SETRANGE32: u32 = 0x0406;
const PBM_SETPOS: u32 = 0x0402;

const SW_SHOW: i32 = 5;
const SM_CXSCREEN: i32 = 0;
const SM_CYSCREEN: i32 = 1;
const LOGPIXELSX: i32 = 88;
const COLOR_BTNFACE: usize = 15;

const MB_OK: u32 = 0x0000_0000;
const MB_YESNO: u32 = 0x0000_0004;
const MB_ICONERROR: u32 = 0x0000_0010;
const MB_ICONQUESTION: u32 = 0x0000_0020;
const MB_ICONWARNING: u32 = 0x0000_0030;
const MB_ICONINFORMATION: u32 = 0x0000_0040;
const IDYES: i32 = 6;

const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;
const OFN_PATHMUSTEXIST: u32 = 0x0000_0800;
const OFN_NOCHANGEDIR: u32 = 0x0000_0008;

// Control IDs
const ID_BTN_BROWSE_ISO: usize = 101;
const ID_BTN_BROWSE_KEYS: usize = 102;
const ID_BTN_BUILD_NRO: usize = 103;
const ID_BTN_BUILD_NSP: usize = 104;
const ID_BTN_BUILD_BOTH: usize = 105;
const ID_BTN_OPEN_OUT: usize = 106;

#[derive(Debug, Clone, Copy)]
enum BuildMode {
    NroOnly,
    NspOnly,
    Both { allow_skip_nsp: bool },
}

enum UiEvent {
    Progress(u32, String),
    Done {
        ok: bool,
        title: String,
        popup_msg: String,
        log_line: String,
    },
}

#[derive(Clone, Copy)]
struct Handles {
    main_hwnd: usize,
    iso_edit: usize,
    iso_status: usize,
    keys_edit: usize,
    btn_browse_iso: usize,
    btn_browse_keys: usize,
    btn_nro: usize,
    btn_nsp: usize,
    btn_both: usize,
    progress_bar: usize,
    progress_label: usize,
    log_edit: usize,
}

struct AppState {
    paths: ProjectPaths,
    handles: Option<Handles>,
    building: bool,
    events: VecDeque<UiEvent>,
    last_log_msg: String,
}

static APP: OnceLock<Mutex<AppState>> = OnceLock::new();

pub fn attach_parent_console() {
    unsafe {
        AttachConsole(0xFFFF_FFFF); // ATTACH_PARENT_PROCESS
    }
}

pub fn show_error_popup(title: &str, msg: &str) {
    let t = to_wide(title);
    let m = to_wide(msg);
    unsafe {
        MessageBoxW(null_mut(), m.as_ptr(), t.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn to_wide_path(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn get_edit_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let got = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if got <= 0 {
            return String::new();
        }
        OsString::from_wide(&buf[..got as usize])
            .to_string_lossy()
            .trim()
            .to_string()
    }
}

fn set_window_text(hwnd: HWND, text: &str) {
    let w = to_wide(text);
    unsafe {
        SetWindowTextW(hwnd, w.as_ptr());
    }
}

fn append_log_to_ctrl(log_hwnd: HWND, line: &str) {
    let mut formatted = line.replace("\r\n", "\n").replace('\n', "\r\n");
    formatted.push_str("\r\n");
    let w = to_wide(&formatted);
    unsafe {
        let len = GetWindowTextLengthW(log_hwnd) as usize;
        SendMessageW(log_hwnd, EM_SETSEL, len, len as isize);
        SendMessageW(log_hwnd, EM_REPLACESEL, 0, w.as_ptr() as isize);
        SendMessageW(log_hwnd, EM_SCROLLCARET, 0, 0);
    }
}

fn set_controls_enabled(h: &Handles, enabled: bool) {
    let b = if enabled { 1 } else { 0 };
    unsafe {
        EnableWindow(h.iso_edit as HWND, b);
        EnableWindow(h.keys_edit as HWND, b);
        EnableWindow(h.btn_browse_iso as HWND, b);
        EnableWindow(h.btn_browse_keys as HWND, b);
        EnableWindow(h.btn_nro as HWND, b);
        EnableWindow(h.btn_nsp as HWND, b);
        EnableWindow(h.btn_both as HWND, b);
    }
}

fn scale(px: i32, dpi: i32) -> i32 {
    (px * dpi + 48) / 96
}

fn create_font(dpi: i32, pt: i32, bold: bool, face: &str) -> HFONT {
    let height = -((pt * dpi + 36) / 72);
    let weight = if bold { 700 } else { 400 };
    let face_w = to_wide(face);
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            1, // DEFAULT_CHARSET
            0,
            0,
            5, // CLEARTYPE_QUALITY
            0,
            face_w.as_ptr(),
        )
    }
}

fn open_file_dialog(owner: HWND, title: &str, filter_pairs: &[(&str, &str)]) -> Option<PathBuf> {
    let mut filter_w: Vec<u16> = Vec::new();
    for (desc, pat) in filter_pairs {
        filter_w.extend(OsStr::new(desc).encode_wide());
        filter_w.push(0);
        filter_w.extend(OsStr::new(pat).encode_wide());
        filter_w.push(0);
    }
    filter_w.push(0);

    let title_w = to_wide(title);
    let mut file_buf = vec![0u16; 32768];

    let mut ofn = OPENFILENAMEW {
        l_struct_size: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwnd_owner: owner,
        h_instance: null_mut(),
        lpstr_filter: filter_w.as_ptr(),
        lpstr_custom_filter: null_mut(),
        n_max_cust_filter: 0,
        n_filter_index: 1,
        lpstr_file: file_buf.as_mut_ptr(),
        n_max_file: file_buf.len() as u32,
        lpstr_file_title: null_mut(),
        n_max_file_title: 0,
        lpstr_initial_dir: null(),
        lpstr_title: title_w.as_ptr(),
        flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        n_file_offset: 0,
        n_file_extension: 0,
        lpstr_def_ext: null(),
        l_cust_data: 0,
        lpfn_hook: 0,
        lp_template_name: null(),
        pv_reserved: null_mut(),
        dw_reserved: 0,
        flags_ex: 0,
    };

    let ok = unsafe { GetOpenFileNameW(&mut ofn) };
    if ok == 0 {
        return None;
    }
    let end = file_buf.iter().position(|&c| c == 0).unwrap_or(file_buf.len());
    Some(PathBuf::from(OsString::from_wide(&file_buf[..end])))
}

fn refresh_iso_status(iso_path: &Path) {
    let state_lock = APP.get().unwrap();
    let (status_hwnd, log_hwnd) = {
        let st = state_lock.lock().unwrap();
        let Some(h) = st.handles.as_ref() else { return };
        (h.iso_status as HWND, h.log_edit as HWND)
    };

    match parse_iso(iso_path) {
        Ok(info) => {
            let badge = if info.is_uk_pal() {
                format!(
                    "Detected: {} ({}) — {} files | .NRO & .NSP work with ZERO keys!",
                    info.game_id,
                    info.region_label,
                    info.root.total_files()
                )
            } else {
                format!(
                    "Detected: {} ({}) — {} files | .NRO: zero keys | .NSP: needs prod.keys",
                    info.game_id,
                    info.region_label,
                    info.root.total_files()
                )
            };
            set_window_text(status_hwnd, &badge);
            append_log_to_ctrl(log_hwnd, &format!("[ISO] {} -> {}", iso_path.display(), badge));
        }
        Err(e) => {
            let short = e.lines().next().unwrap_or(&e);
            set_window_text(status_hwnd, &format!("ISO Error: {short}"));
            append_log_to_ctrl(log_hwnd, &format!("[ISO ERROR] {e}"));
        }
    }
}

fn start_build(mode: BuildMode) {
    let state_lock = APP.get().unwrap();
    let (paths, main_hwnd, iso_text, keys_text, log_hwnd, prog_hwnd, prog_lbl) = {
        let mut st = state_lock.lock().unwrap();
        if st.building {
            return;
        }
        let Some(h) = st.handles else { return };
        let iso_text = get_edit_text(h.iso_edit as HWND);
        let keys_text = get_edit_text(h.keys_edit as HWND);
        if iso_text.is_empty() {
            let hwnd = h.main_hwnd as HWND;
            drop(st);
            let title = to_wide("Select GameCube ISO First");
            let msg = to_wide(
                "Please select your SpongeBob SquarePants Movie GameCube (.iso / .gcm) file first!\n\n\
                 You can either:\n\
                 1. Click 'Browse ISO...' to pick your .iso file, OR\n\
                 2. Drag-and-drop your .iso file directly onto this window.",
            );
            unsafe {
                MessageBoxW(hwnd, msg.as_ptr(), title.as_ptr(), MB_OK | MB_ICONWARNING);
            }
            return;
        }

        st.building = true;
        st.last_log_msg.clear();
        set_controls_enabled(&h, false);
        (
            st.paths.clone(),
            h.main_hwnd,
            iso_text,
            keys_text,
            h.log_edit as HWND,
            h.progress_bar as HWND,
            h.progress_label as HWND,
        )
    };

    unsafe {
        SendMessageW(prog_hwnd, PBM_SETPOS, 0, 0);
    }
    set_window_text(prog_lbl, "0% - Starting...");
    append_log_to_ctrl(
        log_hwnd,
        "------------------------------------------------------------",
    );
    append_log_to_ctrl(log_hwnd, &format!("Starting build ({mode:?})..."));

    let iso_path = PathBuf::from(iso_text);
    let keys_opt = if keys_text.is_empty() {
        None
    } else {
        Some(PathBuf::from(keys_text))
    };

    thread::spawn(move || {
        let push_event = |ev: UiEvent| {
            let lock = APP.get().unwrap();
            {
                let mut st = lock.lock().unwrap();
                st.events.push_back(ev);
            }
            unsafe {
                PostMessageW(main_hwnd as HWND, WM_APP_EVENT, 0, 0);
            }
        };

        let result: Result<(String, String), String> = match mode {
            BuildMode::NroOnly => build_nro(&paths, &iso_path, |pct, msg| {
                push_event(UiEvent::Progress(pct, msg.to_string()));
            })
            .map(|nro| {
                let sz_mb = std::fs::metadata(&nro)
                    .map(|m| m.len() as f64 / (1024.0 * 1024.0))
                    .unwrap_or(0.0);
                (
                    "Switch .NRO Built Successfully!".to_string(),
                    format!(
                        "Your Nintendo Switch Homebrew package is ready!\n\n\
                         File: {}\n\
                         Size: {:.1} MB\n\n\
                         Copy tssm.nro to sd:/switch/tssm/tssm.nro on your Switch (or open in Ryujinx).\n\
                         Use Title Override (hold R while opening a game) for full RAM!",
                        nro.display(),
                        sz_mb
                    ),
                )
            }),
            BuildMode::NspOnly => {
                build_nsp(&paths, &iso_path, keys_opt.as_deref(), false, |pct, msg| {
                    push_event(UiEvent::Progress(pct, msg.to_string()));
                })
                .map(|nsp| {
                    let sz_mb = std::fs::metadata(&nsp)
                        .map(|m| m.len() as f64 / (1024.0 * 1024.0))
                        .unwrap_or(0.0);
                    (
                        "Switch .NSP Built Successfully!".to_string(),
                        format!(
                            "Your installable Nintendo Switch Home Menu package is ready!\n\n\
                             File: {}\n\
                             Size: {:.1} MB\n\n\
                             Install via DBI / Tinfoil / Goldleaf on your Switch, or double-click in Ryujinx!",
                            nsp.display(),
                            sz_mb
                        ),
                    )
                })
            }
            BuildMode::Both { allow_skip_nsp } => build_both(
                &paths,
                &iso_path,
                keys_opt.as_deref(),
                allow_skip_nsp,
                |pct, msg| {
                    push_event(UiEvent::Progress(pct, msg.to_string()));
                },
            )
            .map(|res| {
                let mut msg = format!("Build complete for {}!\n\n", res.iso_summary);
                if let Some(nro) = &res.nro_path {
                    let sz = std::fs::metadata(nro)
                        .map(|m| m.len() as f64 / (1024.0 * 1024.0))
                        .unwrap_or(0.0);
                    msg.push_str(&format!("Homebrew .NRO ({:.1} MB):\n  {}\n\n", sz, nro.display()));
                }
                if let Some(nsp) = &res.nsp_path {
                    let sz = std::fs::metadata(nsp)
                        .map(|m| m.len() as f64 / (1024.0 * 1024.0))
                        .unwrap_or(0.0);
                    msg.push_str(&format!(
                        "Installable .NSP ({:.1} MB):\n  {}\n\n",
                        sz,
                        nsp.display()
                    ));
                }
                if let Some(skip) = &res.nsp_skipped_reason {
                    msg.push_str(&format!("Note on .NSP:\n{skip}\n"));
                } else {
                    msg.push_str("Click 'Open Output Folder' to view your Switch files!");
                }
                ("Switch Build Complete!".to_string(), msg)
            }),
        };

        match result {
            Ok((title, popup_msg)) => {
                push_event(UiEvent::Done {
                    ok: true,
                    title,
                    log_line: format!("[SUCCESS] {}", popup_msg.replace('\n', " | ")),
                    popup_msg,
                });
            }
            Err(err) => {
                push_event(UiEvent::Done {
                    ok: false,
                    title: "Build Failed".to_string(),
                    log_line: format!("[ERROR] {err}"),
                    popup_msg: err,
                });
            }
        }
    });
}

fn drain_events(hwnd: HWND) {
    let state_lock = APP.get().unwrap();
    let events: Vec<UiEvent> = {
        let mut st = state_lock.lock().unwrap();
        st.events.drain(..).collect()
    };

    for ev in events {
        match ev {
            UiEvent::Progress(pct, msg) => {
                let mut st = state_lock.lock().unwrap();
                let is_mb_tick = msg.contains(" / ") && msg.ends_with(" MB");
                let should_log = !is_mb_tick && st.last_log_msg != msg;
                if should_log {
                    st.last_log_msg = msg.clone();
                }
                if let Some(h) = st.handles {
                    unsafe {
                        SendMessageW(h.progress_bar as HWND, PBM_SETPOS, pct as usize, 0);
                    }
                    set_window_text(h.progress_label as HWND, &format!("{pct}% - {msg}"));
                    if should_log {
                        append_log_to_ctrl(h.log_edit as HWND, &format!("[{pct:3}%] {msg}"));
                    }
                }
            }
            UiEvent::Done {
                ok,
                title,
                popup_msg,
                log_line,
            } => {
                {
                    let mut st = state_lock.lock().unwrap();
                    st.building = false;
                    if let Some(h) = st.handles.as_ref() {
                        set_controls_enabled(h, true);
                        unsafe {
                            SendMessageW(
                                h.progress_bar as HWND,
                                PBM_SETPOS,
                                if ok { 100 } else { 0 },
                                0,
                            );
                        }
                        set_window_text(
                            h.progress_label as HWND,
                            if ok { "100% - Done!" } else { "Build failed — see log below." },
                        );
                        append_log_to_ctrl(h.log_edit as HWND, &log_line);
                    }
                }
                let t = to_wide(&title);
                let m = to_wide(&popup_msg);
                let flags = MB_OK | if ok { MB_ICONINFORMATION } else { MB_ICONERROR };
                unsafe {
                    MessageBoxW(hwnd, m.as_ptr(), t.as_ptr(), flags);
                }
            }
        }
    }
}

fn handle_drop_files(hwnd: HWND, hdrop: HDROP) {
    let mut buf = vec![0u16; 32768];
    let len = unsafe { DragQueryFileW(hdrop, 0, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    unsafe {
        DragFinish(hdrop);
    }
    if len == 0 {
        return;
    }
    let dropped = PathBuf::from(OsString::from_wide(&buf[..len]));
    let ext = dropped
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let state_lock = APP.get().unwrap();
    let (building, iso_edit, keys_edit, log_edit) = {
        let st = state_lock.lock().unwrap();
        let Some(h) = st.handles.as_ref() else { return };
        (
            st.building,
            h.iso_edit as HWND,
            h.keys_edit as HWND,
            h.log_edit as HWND,
        )
    };
    if building {
        return;
    }

    if ext == "keys" || ext == "dat" {
        set_window_text(keys_edit, &dropped.display().to_string());
        append_log_to_ctrl(
            log_edit,
            &format!("[KEYS] Loaded dropped prod.keys: {}", dropped.display()),
        );
    } else {
        set_window_text(iso_edit, &dropped.display().to_string());
        refresh_iso_status(&dropped);
    }
    unsafe {
        SetForegroundWindow(hwnd);
    }
}

fn open_output_folder(hwnd: HWND) {
    let out_dir = {
        let st = APP.get().unwrap().lock().unwrap();
        st.paths.out_dir.clone()
    };
    let _ = std::fs::create_dir_all(&out_dir);
    let verb = to_wide("open");
    let path_w = to_wide_path(&out_dir);
    unsafe {
        ShellExecuteW(
            hwnd,
            verb.as_ptr(),
            path_w.as_ptr(),
            null(),
            null(),
            SW_SHOW,
        );
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = wparam & 0xFFFF;
            match id {
                ID_BTN_BROWSE_ISO => {
                    if let Some(p) = open_file_dialog(
                        hwnd,
                        "Select SpongeBob Movie GameCube ISO / GCM",
                        &[
                            ("GameCube Disc Images (*.iso;*.gcm)", "*.iso;*.gcm"),
                            ("All Files (*.*)", "*.*"),
                        ],
                    ) {
                        if let Some(lock) = APP.get() {
                            let edit = {
                                let st = lock.lock().unwrap();
                                st.handles.as_ref().map(|h| h.iso_edit as HWND)
                            };
                            if let Some(e) = edit {
                                set_window_text(e, &p.display().to_string());
                            }
                        }
                        refresh_iso_status(&p);
                    }
                    0
                }
                ID_BTN_BROWSE_KEYS => {
                    if let Some(p) = open_file_dialog(
                        hwnd,
                        "Select Switch prod.keys (Optional — needed only for USA .NSP)",
                        &[
                            ("Switch Keys (prod.keys;*.keys;*.dat)", "prod.keys;*.keys;*.dat"),
                            ("All Files (*.*)", "*.*"),
                        ],
                    ) {
                        if let Some(lock) = APP.get() {
                            let (edit, log) = {
                                let st = lock.lock().unwrap();
                                (
                                    st.handles.as_ref().map(|h| h.keys_edit as HWND),
                                    st.handles.as_ref().map(|h| h.log_edit as HWND),
                                )
                            };
                            if let Some(e) = edit {
                                set_window_text(e, &p.display().to_string());
                            }
                            if let Some(l) = log {
                                append_log_to_ctrl(
                                    l,
                                    &format!("[KEYS] Selected prod.keys: {}", p.display()),
                                );
                            }
                        }
                    }
                    0
                }
                ID_BTN_BUILD_NRO => {
                    start_build(BuildMode::NroOnly);
                    0
                }
                ID_BTN_BUILD_NSP => {
                    start_build(BuildMode::NspOnly);
                    0
                }
                ID_BTN_BUILD_BOTH => {
                    start_build(BuildMode::Both {
                        allow_skip_nsp: true,
                    });
                    0
                }
                ID_BTN_OPEN_OUT => {
                    open_output_folder(hwnd);
                    0
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        WM_DROPFILES => {
            handle_drop_files(hwnd, wparam as HDROP);
            0
        }
        WM_APP_EVENT => {
            drain_events(hwnd);
            0
        }
        WM_APP_AUTOBUILD => {
            start_build(BuildMode::Both {
                allow_skip_nsp: true,
            });
            0
        }
        WM_CLOSE => {
            let is_building = APP
                .get()
                .map(|l| l.lock().unwrap().building)
                .unwrap_or(false);
            if is_building {
                let t = to_wide("Build In Progress");
                let m = to_wide("A Switch package build is currently running. Close anyway?");
                if MessageBoxW(hwnd, m.as_ptr(), t.as_ptr(), MB_YESNO | MB_ICONQUESTION) != IDYES {
                    return 0;
                }
            }
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub fn run_gui(paths: ProjectPaths, dropped_iso: Option<PathBuf>) -> i32 {
    // Install panic hook so even an unexpected panic pops up visibly instead of disappearing.
    std::panic::set_hook(Box::new(|info| {
        show_error_popup(
            "TSSM Switch Builder — Unexpected Error",
            &format!("An internal error occurred:\n\n{info}"),
        );
    }));

    let _ = APP.set(Mutex::new(AppState {
        paths: paths.clone(),
        handles: None,
        building: false,
        events: VecDeque::new(),
        last_log_msg: String::new(),
    }));

    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dw_size: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dw_icc: 0x0000_0020 | 0x0000_4000, // ICC_PROGRESS_CLASS | ICC_STANDARD_CLASSES
        };
        InitCommonControlsEx(&icc);

        let hinst = GetModuleHandleW(null());
        let hdc = GetDC(null_mut());
        let dpi = if !hdc.is_null() {
            let d = GetDeviceCaps(hdc, LOGPIXELSX);
            ReleaseDC(null_mut(), hdc);
            if d > 0 { d } else { 96 }
        } else {
            96
        };

        let class_name = to_wide("TssmSwitchBuilderRustWnd");
        let app_icon = {
            let ic = LoadIconW(hinst, 1usize as *const u16);
            if !ic.is_null() {
                ic
            } else {
                LoadIconW(null_mut(), 32512usize as *const u16)
            }
        };
        let cursor = LoadCursorW(null_mut(), 32512usize as *const u16);

        let wc = WNDCLASSEXW {
            cb_size: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfn_wnd_proc: wnd_proc,
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: hinst,
            h_icon: app_icon,
            h_cursor: cursor,
            hbr_background: (COLOR_BTNFACE + 1) as HBRUSH,
            lpsz_menu_name: null(),
            lpsz_class_name: class_name.as_ptr(),
            h_icon_sm: app_icon,
        };

        if RegisterClassExW(&wc) == 0 {
            let err = GetLastError();
            show_error_popup(
                "TSSM Switch Builder",
                &format!("Failed to register window class (Win32 error {err})."),
            );
            return 1;
        }

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE;
        let ex_style = WS_EX_ACCEPTFILES;
        let client_w = scale(680, dpi);
        let client_h = scale(540, dpi);
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: client_w,
            bottom: client_h,
        };
        AdjustWindowRectEx(&mut rect, style, 0, ex_style);
        let win_w = rect.right - rect.left;
        let win_h = rect.bottom - rect.top;
        let scr_w = GetSystemMetrics(SM_CXSCREEN);
        let scr_h = GetSystemMetrics(SM_CYSCREEN);
        let pos_x = ((scr_w - win_w) / 2).max(20);
        let pos_y = ((scr_h - win_h) / 2).max(20);

        let title = to_wide("The SpongeBob SquarePants Movie — Switch Builder (.NRO / .NSP)");
        let hwnd = CreateWindowExW(
            ex_style,
            class_name.as_ptr(),
            title.as_ptr(),
            style,
            pos_x,
            pos_y,
            win_w,
            win_h,
            null_mut(),
            null_mut(),
            hinst,
            null_mut(),
        );
        if hwnd.is_null() {
            let err = GetLastError();
            show_error_popup(
                "TSSM Switch Builder",
                &format!("Failed to create application window (Win32 error {err})."),
            );
            return 1;
        }

        DragAcceptFiles(hwnd, 1);

        let font_title = create_font(dpi, 13, true, "Segoe UI");
        let font_ui = create_font(dpi, 9, false, "Segoe UI");
        let font_bold = create_font(dpi, 9, true, "Segoe UI");
        let font_mono = create_font(dpi, 9, false, "Consolas");

        let s = |v: i32| scale(v, dpi);
        let set_font = |ctrl: HWND, f: HFONT| {
            SendMessageW(ctrl, WM_SETFONT, f as usize, 1);
        };

        let cls_static = to_wide("STATIC");
        let cls_edit = to_wide("EDIT");
        let cls_button = to_wide("BUTTON");
        let cls_prog = to_wide("msctls_progress32");

        let make_static = |x, y, w, h, text: &str, font: HFONT| -> HWND {
            let tw = to_wide(text);
            let c = CreateWindowExW(
                0,
                cls_static.as_ptr(),
                tw.as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_LEFT | SS_NOPREFIX,
                s(x),
                s(y),
                s(w),
                s(h),
                hwnd,
                null_mut(),
                hinst,
                null_mut(),
            );
            set_font(c, font);
            c
        };

        let make_button = |x, y, w, h, text: &str, id: usize, def: bool, font: HFONT| -> HWND {
            let tw = to_wide(text);
            let bs = if def { BS_DEFPUSHBUTTON } else { BS_PUSHBUTTON };
            let c = CreateWindowExW(
                0,
                cls_button.as_ptr(),
                tw.as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | bs,
                s(x),
                s(y),
                s(w),
                s(h),
                hwnd,
                id as HMENU,
                hinst,
                null_mut(),
            );
            set_font(c, font);
            c
        };

        // Header
        make_static(
            18,
            14,
            644,
            26,
            "The SpongeBob SquarePants Movie — Nintendo Switch Builder",
            font_title,
        );
        make_static(
            18,
            42,
            644,
            34,
            "Drag & drop your GameCube .iso / .gcm file onto this window (or click Browse ISO), then click a Build button.\r\nWorks with BOTH USA (GGVE78) and UK/PAL (GGVP78) GameCube ISOs — no extra tools required!",
            font_ui,
        );

        // Row 1: GameCube ISO
        make_static(18, 86, 644, 18, "1. GameCube ISO / GCM File (Required):", font_bold);
        let iso_edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            cls_edit.as_ptr(),
            to_wide("").as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL,
            s(18),
            s(106),
            s(524),
            s(25),
            hwnd,
            null_mut(),
            hinst,
            null_mut(),
        );
        set_font(iso_edit, font_ui);
        let btn_browse_iso = make_button(550, 105, 112, 27, "Browse ISO...", ID_BTN_BROWSE_ISO, false, font_ui);

        let iso_status = make_static(
            18,
            135,
            644,
            20,
            "No ISO selected yet — drag & drop a .iso file here or click Browse ISO...",
            font_ui,
        );

        // Row 2: Optional prod.keys
        make_static(
            18,
            164,
            644,
            18,
            "2. Switch prod.keys (Optional — ZERO keys needed for .NRO or UK .NSP; only needed for USA .NSP):",
            font_bold,
        );
        let keys_edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            cls_edit.as_ptr(),
            to_wide("").as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL,
            s(18),
            s(184),
            s(524),
            s(25),
            hwnd,
            null_mut(),
            hinst,
            null_mut(),
        );
        set_font(keys_edit, font_ui);
        let btn_browse_keys = make_button(550, 183, 112, 27, "Browse Keys...", ID_BTN_BROWSE_KEYS, false, font_ui);

        // Row 3: Action Buttons
        let btn_nro = make_button(18, 224, 175, 36, "Build .NRO (Homebrew)", ID_BTN_BUILD_NRO, false, font_bold);
        let btn_nsp = make_button(201, 224, 175, 36, "Build .NSP (Home Menu)", ID_BTN_BUILD_NSP, false, font_bold);
        let btn_both = make_button(384, 224, 152, 36, "Build Both", ID_BTN_BUILD_BOTH, true, font_bold);
        let _btn_open_out = make_button(544, 224, 118, 36, "Open Output", ID_BTN_OPEN_OUT, false, font_ui);

        // Row 4: Progress bar + status
        let progress_label = make_static(18, 272, 644, 20, "Ready.", font_bold);
        let progress_bar = CreateWindowExW(
            0,
            cls_prog.as_ptr(),
            to_wide("").as_ptr(),
            WS_CHILD | WS_VISIBLE,
            s(18),
            s(294),
            s(644),
            s(22),
            hwnd,
            null_mut(),
            hinst,
            null_mut(),
        );
        SendMessageW(progress_bar, PBM_SETRANGE32, 0, 100);
        SendMessageW(progress_bar, PBM_SETPOS, 0, 0);

        // Row 5: Log Box
        make_static(18, 324, 644, 18, "Build Log:", font_bold);
        let log_edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            cls_edit.as_ptr(),
            to_wide("").as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY,
            s(18),
            s(344),
            s(644),
            s(180),
            hwnd,
            null_mut(),
            hinst,
            null_mut(),
        );
        set_font(log_edit, font_mono);
        SendMessageW(log_edit, EM_SETLIMITTEXT, 0, 0);

        {
            let mut st = APP.get().unwrap().lock().unwrap();
            st.handles = Some(Handles {
                main_hwnd: hwnd as usize,
                iso_edit: iso_edit as usize,
                iso_status: iso_status as usize,
                keys_edit: keys_edit as usize,
                btn_browse_iso: btn_browse_iso as usize,
                btn_browse_keys: btn_browse_keys as usize,
                btn_nro: btn_nro as usize,
                btn_nsp: btn_nsp as usize,
                btn_both: btn_both as usize,
                progress_bar: progress_bar as usize,
                progress_label: progress_label as usize,
                log_edit: log_edit as usize,
            });
        }

        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);

        append_log_to_ctrl(
            log_edit,
            &format!("TSSM Switch Builder v2.0 (Rust Native) | Project root: {}", paths.root.display()),
        );
        append_log_to_ctrl(
            log_edit,
            &format!("Output folder: {}", paths.out_dir.display()),
        );

        if !paths.templates_present {
            let warn = format!(
                "WARNING: Missing Switch engine templates in {}!\n\
                 Expected:\n  - {}\n  - {}\n\n\
                 Keep TSSM-Builder.exe inside the downloaded repository folder.",
                paths.root.join("tools").join("switch").display(),
                paths.engine_nro.display(),
                paths.nsp_template.display()
            );
            append_log_to_ctrl(log_edit, &warn);
            let t = to_wide("Missing Engine Templates");
            let m = to_wide(&warn);
            MessageBoxW(hwnd, m.as_ptr(), t.as_ptr(), MB_OK | MB_ICONWARNING);
        } else {
            append_log_to_ctrl(
                log_edit,
                "[OK] Switch engine templates found (tssm_engine.nro & tssm_nsp_template.bin).",
            );
        }

        // Auto-detect prod.keys
        if let Some(kpath) = auto_find_prod_keys(&paths.root) {
            set_window_text(keys_edit, &kpath.display().to_string());
            append_log_to_ctrl(
                log_edit,
                &format!("[OK] Auto-detected Switch prod.keys: {}", kpath.display()),
            );
        }

        // Preload dropped ISO (if dragged onto TSSM-Builder.exe) OR auto-scan folder!
        if let Some(iso) = dropped_iso {
            set_window_text(iso_edit, &iso.display().to_string());
            refresh_iso_status(&iso);
            if paths.templates_present && parse_iso(&iso).is_ok() {
                append_log_to_ctrl(
                    log_edit,
                    "[AUTO] ISO dropped onto TSSM-Builder.exe — starting automatic build!",
                );
                PostMessageW(hwnd, WM_APP_AUTOBUILD, 0, 0);
            }
        } else if let Some((found_iso, _)) = auto_find_iso(&paths.root) {
            set_window_text(iso_edit, &found_iso.display().to_string());
            refresh_iso_status(&found_iso);
        } else {
            append_log_to_ctrl(
                log_edit,
                "Tip: Drag & drop your SpongeBob Movie .iso onto this window (or click Browse ISO...), then click Build!",
            );
        }

        let mut msg = std::mem::zeroed::<MSG>();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        msg.w_param as i32
    }
}
