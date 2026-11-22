//! Embeds the built web UI from `ui/dist/` as a table of files. Without a UI
//! build the table is empty and the server answers `/ui/` with a page that
//! says so.

use std::{
    error::Error,
    fmt::Write as _,
    path::{Path, PathBuf},
};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=ui/dist");
    let dist = Path::new(&std::env::var("CARGO_MANIFEST_DIR")?).join("ui/dist");
    let mut files = Vec::new();
    if dist.is_dir() {
        collect(&dist, &mut files)?;
    }
    files.sort();
    let mut table = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for path in files {
        let name = path
            .strip_prefix(&dist)?
            .to_str()
            .ok_or("a file name in ui/dist is not UTF-8")?
            .replace('\\', "/");
        let full = path.to_str().ok_or("the path of ui/dist is not UTF-8")?;
        writeln!(table, "    ({name:?}, include_bytes!({full:?})),")?;
    }
    table.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR")?).join("ui_files.rs");
    std::fs::write(out, table)?;
    Ok(())
}

/// The files under `dir`, without dotfiles such as `.gitkeep`.
fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        if path.is_dir() {
            collect(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}
