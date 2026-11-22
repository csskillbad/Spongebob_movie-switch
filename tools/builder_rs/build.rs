use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=icon.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        let res_obj = out_dir.join("app_res.o");

        // Try x86_64-w64-mingw32-windres (Linux cross-compile) or windres (native Windows MinGW).
        let windres_candidates = ["x86_64-w64-mingw32-windres", "windres"];
        let mut compiled = false;
        for tool in &windres_candidates {
            if let Ok(status) = Command::new(tool)
                .args(["-O", "coff", "app.rc", "-o"])
                .arg(&res_obj)
                .status()
            {
                if status.success() {
                    println!("cargo:rustc-link-arg={}", res_obj.display());
                    compiled = true;
                    break;
                }
            }
        }
        if !compiled {
            println!("cargo:warning=windres not found; building Windows exe without embedded icon/manifest");
        }

        // Standard Windows system libraries only (present on every Windows 7/8/10/11 PC).
        for lib in ["user32", "gdi32", "comdlg32", "shell32", "comctl32"] {
            println!("cargo:rustc-link-lib={lib}");
        }
    }
}
