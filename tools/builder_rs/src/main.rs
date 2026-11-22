#![cfg_attr(windows, windows_subsystem = "windows")]

mod builder;
mod crypto;
mod gcm;
mod romfs;
#[cfg(windows)]
mod win_gui;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use builder::{
    auto_find_iso, auto_find_prod_keys, build_both, build_nro, build_nsp, ProjectPaths,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliAction {
    AutoDefault,
    BuildNro,
    BuildNsp,
    BuildAll,
    Help,
}

fn print_help() {
    println!(
        "TSSM Switch Builder v2.0 (Rust Native)\n\
         Builds Nintendo Switch .NRO (Homebrew) and .NSP (Home Menu) packages\n\
         for The SpongeBob SquarePants Movie from your GameCube ISO (USA GGVE78 or UK GGVP78).\n\
         \n\
         Usage:\n\
           Double-click TSSM-Builder.exe            Launch interactive Windows GUI\n\
           Drag & drop <game.iso> onto the .exe     Auto-build .NRO & .NSP with progress window\n\
           tssm-builder [OPTIONS] [ISO_PATH]\n\
         \n\
         Options:\n\
           --nro, --build-nro       Build Switch Homebrew target/switch/tssm.nro (ZERO keys needed)\n\
           --nsp, --build-nsp       Build Switch Home Menu target/switch/nsp/0100780012340000.nsp\n\
           --all, --build-all       Build both .NRO and .NSP\n\
           --keys <PATH>            Path to Switch prod.keys (needed only for USA .NSP)\n\
           --force-universal        Force universal NCA3/IVFC/CNMT .NSP path (requires prod.keys)\n\
           -h, --help               Show this help message"
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let paths = ProjectPaths::discover();

    let mut action = CliAction::AutoDefault;
    let mut iso_arg: Option<PathBuf> = None;
    let mut keys_arg: Option<PathBuf> = None;
    let mut force_universal = false;
    let mut has_explicit_cli_flag = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--nro" | "--build-nro" => {
                action = CliAction::BuildNro;
                has_explicit_cli_flag = true;
            }
            "--nsp" | "--build-nsp" => {
                action = CliAction::BuildNsp;
                has_explicit_cli_flag = true;
            }
            "--all" | "--build-all" => {
                action = CliAction::BuildAll;
                has_explicit_cli_flag = true;
            }
            "--force-universal" => {
                force_universal = true;
                has_explicit_cli_flag = true;
            }
            "--keys" => {
                has_explicit_cli_flag = true;
                if i + 1 < args.len() {
                    i += 1;
                    keys_arg = Some(PathBuf::from(&args[i]));
                }
            }
            "-h" | "--help" => {
                action = CliAction::Help;
                has_explicit_cli_flag = true;
            }
            other if !other.starts_with('-') => {
                iso_arg = Some(PathBuf::from(other));
            }
            unknown => {
                #[cfg(windows)]
                win_gui::attach_parent_console();
                eprintln!("Unknown option: {unknown}");
                print_help();
                return ExitCode::from(2);
            }
        }
        i += 1;
    }

    // On Windows: launch the native GUI unless explicit CLI flags (--build-*, --help, etc.) were passed.
    // Dragging an ISO onto TSSM-Builder.exe passes just `argv[1] = "C:\path\to\game.iso"`,
    // so `has_explicit_cli_flag == false` and it opens the GUI with live progress + auto-build!
    #[cfg(windows)]
    if !has_explicit_cli_flag {
        let code = win_gui::run_gui(paths, iso_arg);
        return if code == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        };
    }

    #[cfg(windows)]
    win_gui::attach_parent_console();

    if action == CliAction::Help {
        print_help();
        return ExitCode::SUCCESS;
    }

    // On Linux without explicit flags & without a TTY (e.g. double-clicked in file manager), try zenity!
    #[cfg(not(windows))]
    if !has_explicit_cli_flag && iso_arg.is_none() && !is_stdout_tty() {
        if let Some(code) = try_linux_zenity_gui(&paths) {
            return code;
        }
    }

    let iso_path = match iso_arg {
        Some(p) => p,
        None => match auto_find_iso(&paths.root) {
            Some((p, info)) => {
                println!(
                    "[AUTO] Found GameCube ISO: {} ({}, {})",
                    p.display(),
                    info.game_id,
                    info.region_label
                );
                p
            }
            None => {
                eprintln!(
                    "Error: No SpongeBob Movie GameCube ISO (.iso / .gcm) found in '{}'.\n\
                     Pass the ISO path as an argument: tssm-builder --all /path/to/spongebob.iso",
                    paths.root.display()
                );
                return ExitCode::from(1);
            }
        },
    };

    let keys_path = keys_arg.or_else(|| auto_find_prod_keys(&paths.root));
    let mut last_printed = String::new();
    let mut progress_cb = |pct: u32, msg: &str| {
        if msg != last_printed {
            println!("[{pct:3}%] {msg}");
            last_printed = msg.to_string();
        }
    };

    let res: Result<(), String> = match action {
        CliAction::BuildNro => build_nro(&paths, &iso_path, &mut progress_cb).map(|p| {
            println!("SUCCESS: {}", p.display());
        }),
        CliAction::BuildNsp => build_nsp(
            &paths,
            &iso_path,
            keys_path.as_deref(),
            force_universal,
            &mut progress_cb,
        )
        .map(|p| {
            println!("SUCCESS: {}", p.display());
        }),
        CliAction::BuildAll | CliAction::AutoDefault => {
            if force_universal {
                build_nro(&paths, &iso_path, |pct, m| progress_cb(pct / 2, m)).and_then(|nro| {
                    println!("SUCCESS (.NRO): {}", nro.display());
                    build_nsp(
                        &paths,
                        &iso_path,
                        keys_path.as_deref(),
                        true,
                        |pct, m| progress_cb(50 + pct / 2, m),
                    )
                    .map(|p| println!("SUCCESS (.NSP): {}", p.display()))
                })
            } else {
                build_both(&paths, &iso_path, keys_path.as_deref(), true, &mut progress_cb).map(
                    |r| {
                        if let Some(nro) = r.nro_path {
                            println!("SUCCESS (.NRO): {}", nro.display());
                        }
                        if let Some(nsp) = r.nsp_path {
                            println!("SUCCESS (.NSP): {}", nsp.display());
                        }
                        if let Some(skip) = r.nsp_skipped_reason {
                            println!("NOTE (.NSP skipped): {skip}");
                        }
                    },
                )
            }
        }
        CliAction::Help => unreachable!(),
    };

    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ERROR: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(windows))]
fn is_stdout_tty() -> bool {
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    unsafe { isatty(1) != 0 }
}

#[cfg(not(windows))]
fn try_linux_zenity_gui(paths: &ProjectPaths) -> Option<ExitCode> {
    use std::process::Command;

    if env::var("DISPLAY").is_err() && env::var("WAYLAND_DISPLAY").is_err() {
        return None;
    }
    // Check if zenity is installed
    if Command::new("zenity").arg("--version").output().is_err() {
        return None;
    }

    let iso_path = if let Some((found, _)) = auto_find_iso(&paths.root) {
        found
    } else {
        let out = Command::new("zenity")
            .args([
                "--file-selection",
                "--title=Select SpongeBob Movie GameCube ISO (.iso / .gcm)",
                "--file-filter=GameCube ISO (*.iso *.gcm) | *.iso *.ISO *.gcm *.GCM",
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return Some(ExitCode::SUCCESS);
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            return Some(ExitCode::SUCCESS);
        }
        PathBuf::from(s)
    };

    let keys_path = auto_find_prod_keys(&paths.root);
    let res = build_both(paths, &iso_path, keys_path.as_deref(), true, |_, _| {});
    match res {
        Ok(r) => {
            let mut text = format!("Build succeeded for {}!\n\n", r.iso_summary);
            if let Some(nro) = r.nro_path {
                text.push_str(&format!(".NRO: {}\n", nro.display()));
            }
            if let Some(nsp) = r.nsp_path {
                text.push_str(&format!(".NSP: {}\n", nsp.display()));
            }
            if let Some(skip) = r.nsp_skipped_reason {
                text.push_str(&format!("\n{skip}"));
            }
            let _ = Command::new("zenity")
                .args(["--info", "--title=TSSM Switch Builder", &format!("--text={text}")])
                .status();
            Some(ExitCode::SUCCESS)
        }
        Err(e) => {
            let _ = Command::new("zenity")
                .args(["--error", "--title=TSSM Switch Builder", &format!("--text={e}")])
                .status();
            Some(ExitCode::from(1))
        }
    }
}
