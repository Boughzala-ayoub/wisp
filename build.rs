//! Embeds the bundled themes in `themes/` so the exe needs no files beside
//! them, and puts the bundled ConPTY next to the exe if bootstrap fetched it.

use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
    embed_themes();
    copy_conpty();
    compile_shaders();
}

/// Compiles the HLSL with the Windows SDK's fxc.exe so startup doesn't have to.
/// Without the SDK, the renderer compiles them at runtime instead.
fn compile_shaders() {
    println!("cargo:rerun-if-changed=src/shader.hlsl");
    println!("cargo::rustc-check-cfg=cfg(precompiled_shaders)");
    let Some(fxc) = find_fxc() else {
        println!("cargo:warning=fxc.exe not found; shaders will compile at startup");
        return;
    };
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    for (entry, profile) in [("vs_main", "vs_4_0"), ("ps_main", "ps_4_0")] {
        let status = std::process::Command::new(&fxc)
            .args(["/nologo", "/O3", "/T", profile, "/E", entry, "/Fo"])
            .arg(out.join(format!("{entry}.cso")))
            .arg("src/shader.hlsl")
            .status();
        if !status.is_ok_and(|s| s.success()) {
            println!("cargo:warning=fxc couldn't compile {entry}; shaders will compile at startup");
            return;
        }
    }
    println!("cargo:rustc-cfg=precompiled_shaders");
}

/// The newest fxc.exe under the Windows SDK's bin folder.
fn find_fxc() -> Option<PathBuf> {
    let program_files = env::var_os("ProgramFiles(x86)")?;
    let bin = Path::new(&program_files).join("Windows Kits").join("10").join("bin");
    let mut versions: Vec<PathBuf> = fs::read_dir(bin).ok()?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    versions.sort();
    versions.into_iter().rev().map(|v| v.join("x64").join("fxc.exe")).find(|fxc| fxc.is_file())
}

fn embed_themes() {
    println!("cargo:rerun-if-changed=themes");
    let mut themes: Vec<_> = fs::read_dir("themes")
        .expect("themes/ is missing")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_file())
        .collect();
    themes.sort();
    let mut code = String::from(
        "/// Every bundled theme as (name, contents), sorted by name.\npub static THEMES: &[(&str, &str)] = &[\n",
    );
    for path in themes {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let absolute = fs::canonicalize(&path).unwrap();
        code += &format!("    ({name:?}, include_str!({absolute:?})),\n");
    }
    code += "];\n";
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("themes.rs"), code).unwrap();
}

/// conpty.dll looks for OpenConsole.exe beside itself, and Wisp looks for
/// conpty.dll beside wisp.exe. Tests run from `deps/`, so it goes there too.
fn copy_conpty() {
    let source = Path::new(".tools/conpty");
    println!("cargo:rerun-if-changed=.tools/conpty");
    if !source.join("conpty.dll").exists() {
        return; // Wisp falls back to the ConPTY built into Windows.
    }
    // OUT_DIR is target/<profile>/build/wisp-<hash>/out.
    let profile_dir: PathBuf = PathBuf::from(env::var("OUT_DIR").unwrap()).ancestors().nth(3).unwrap().to_path_buf();
    for dir in [profile_dir.clone(), profile_dir.join("deps")] {
        for file in ["conpty.dll", "OpenConsole.exe"] {
            let _ = fs::create_dir_all(&dir);
            if let Err(e) = fs::copy(source.join(file), dir.join(file)) {
                println!("cargo:warning=couldn't copy {file} to {}: {e}", dir.display());
            }
        }
    }
}
