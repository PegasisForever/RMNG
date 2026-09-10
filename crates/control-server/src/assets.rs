//! Runtime payload assets. The Docker image ships everything the control-server
//! distributes (and the frontend it serves) on the filesystem under
//! [`INSTALL_DIR`] — the `clone-daemon` and `agent-wrapper` binaries and `static/`
//! (the built frontend). Everything is stored PLAIN (no gzip — registry pushes
//! compress layers anyway) and nothing is compiled into the binary (rust-embed is
//! gone): a payload is looked up at use time with a two-entry search path — the
//! image install dir first, then the repo dev dir — so `cargo run -p control-server`
//! from a checkout picks up locally staged payloads and the dev frontend build
//! without any configuration.

use std::path::{Path, PathBuf};

/// Where the Docker image installs the payloads + frontend (Dockerfile runtime stage).
pub const INSTALL_DIR: &str = "/usr/local/share/rmng";

/// Dev payload dir inside the repo (gitignored; stage plain `clone-daemon` /
/// `agent-wrapper` here by hand or via a local build). Compile-time absolute so it
/// resolves regardless of CWD; in the image the baked build path simply doesn't
/// exist and the search falls through.
const DEV_PAYLOAD_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/embedded-bin");

/// Dev frontend build output (`bun run build` in `frontend/`).
const DEV_STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../frontend/build/client");

/// Read the payload file `name` (a plain filename, e.g. `clone-daemon` or
/// `agent-wrapper`), if present + non-empty. Missing payloads are tolerated by
/// design: callers warn and fall back (e.g. a dev checkout without a payload
/// staged skips it).
pub fn payload(name: &str) -> Option<Vec<u8>> {
    let path = [Path::new(INSTALL_DIR), Path::new(DEV_PAYLOAD_DIR)]
        .iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())?;
    let bytes = std::fs::read(&path).ok()?;
    (!bytes.is_empty()).then_some(bytes)
}

/// Resolve the frontend dir: `<INSTALL_DIR>/static` in the image, else the repo dev
/// build. `None` when neither has an `index.html` (the web layer serves a 404 hint;
/// the API stays up).
pub fn static_dir() -> Option<PathBuf> {
    [
        Path::new(INSTALL_DIR).join("static"),
        PathBuf::from(DEV_STATIC_DIR),
    ]
    .into_iter()
    .find(|p| p.join("index.html").is_file())
}
