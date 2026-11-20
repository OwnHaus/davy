use std::path::{Path, PathBuf};

/// A fresh path under `target/`, not created yet. Canonical, so it equals the
/// data directory setup and doctor resolve it to.
pub fn scratch(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize()
        .unwrap()
        .join(format!("test-{name}-{}", uuid::Uuid::new_v4()))
}

pub fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("e2e/fixtures")
            .join(path),
    )
    .unwrap()
}
