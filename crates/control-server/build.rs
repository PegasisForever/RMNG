//! Guarantee the embedded-frontend folder exists so `rust_embed` compiles even on
//! a clean checkout where the frontend hasn't been built. The real assets come
//! from `bun run build` (→ `frontend/build/client`); if they're missing we drop a
//! placeholder `index.html` and warn, rather than failing the build.

use std::path::Path;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    // Embedded-binaries folder (clone-daemon.gz, agent-wrapper.gz) — populated by
    // the build pipeline before this crate is built; ensure it exists so rust-embed
    // compiles on a clean checkout (empty → no embedded binaries, graceful).
    let bin_dir = Path::new(&manifest).join("embedded-bin");
    let _ = std::fs::create_dir_all(&bin_dir);
    println!("cargo:rerun-if-changed={}", bin_dir.display());

    let dir = Path::new(&manifest).join("../../frontend/build/client");
    if !dir.join("index.html").exists() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            dir.join("index.html"),
            "<!doctype html><meta charset=utf-8><title>RMNG</title>\
             <body style=\"font-family:sans-serif;padding:2rem\">Frontend not built. \
             Run <code>bun run build</code> in <code>rmng/frontend</code>, then rebuild.</body>",
        );
        println!(
            "cargo:warning=frontend not built (frontend/build/client missing) — embedded a placeholder; run `bun run build` in rmng/frontend"
        );
    }
    println!("cargo:rerun-if-changed={}", dir.display());
}
