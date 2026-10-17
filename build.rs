//! Embeds the bundled themes in `themes/` so the exe needs no files beside
//! them, and puts the bundled ConPTY next to the exe if bootstrap fetched it.

use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
    embed_themes();
    copy_conpty();
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
