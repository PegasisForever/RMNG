//! Artifacts embedded into the control-server (gzipped) so it's a single
//! self-contained bundle that can bootstrap clones with no external artifact
//! management: the **clone-daemon** (capture/input pipe), the **agent-wrapper**
//! (Bun-compiled Claude Agent SDK service), and the patched **gnome-shell-deb**
//! (shell-01 hide screen-sharing indicator + shell-03 enable `org.gnome.Shell.Eval`
//! for window-management MCP tools). The build pipeline (`cs-build-ct.sh`) drops
//! `embedded-bin/<name>.gz` before the control-server is built; `build.rs`
//! guarantees the folder exists so this compiles even on a clean checkout.

use std::io::Read;

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/embedded-bin"]
struct Binaries;

/// Decompress the embedded `<name>.gz` binary, if present + non-empty.
pub fn embedded_binary(name: &str) -> Option<Vec<u8>> {
    let f = Binaries::get(&format!("{name}.gz"))?;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(&f.data[..]).read_to_end(&mut out).ok()?;
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// When the patched gnome-shell deb is staged (build pipeline ran), the embed
    /// must decompress to a real Debian package — guards the `<name>.gz` naming
    /// (`gnome-shell-deb`) and the gzip round-trip used by `stage_binary`. On a clean
    /// checkout the deb isn't staged, so absence is acceptable (skips).
    #[test]
    fn gnome_shell_deb_round_trips_when_embedded() {
        match embedded_binary("gnome-shell-deb") {
            // `.deb` is an `ar` archive — first member is "debian-binary".
            Some(bytes) => assert!(
                bytes.starts_with(b"!<arch>\ndebian-binary"),
                "embedded gnome-shell-deb is not a valid .deb (got {} bytes, head {:?})",
                bytes.len(),
                &bytes[..bytes.len().min(16)]
            ),
            None => eprintln!("gnome-shell-deb not embedded in this build — skipping"),
        }
    }
}
