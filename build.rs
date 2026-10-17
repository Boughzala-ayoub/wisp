//! Embeds the bundled themes in `themes/` so the exe needs no files beside it.

use std::path::Path;
use std::{env, fs};

fn main() {
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
