//! On-disk side stores that are **not** part of `ControlState`/SSE: per-host
//! editor notes (`data/notes/<id>.json`), image uploads (`data/uploads/`), and
//! detector-feedback records (`data/detector-feedback/`). Ports `notes.server.ts`,
//! `uploads.server.ts`, `detector-feedback.server.ts`, `paths.server.ts`.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

const MAX_UPLOAD_BYTES: usize = 15 * 1024 * 1024;

/// A host/note id is a DNS label (path-traversal guard).
pub fn is_safe_id(id: &str) -> bool {
    crate::provision::is_dns_label(id)
}

/// 16 random bytes as hex, from `/dev/urandom` (Linux). Used for upload/feedback ids.
fn rand_hex() -> String {
    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// --- notes -----------------------------------------------------------------

fn note_path(data_dir: &str, id: &str) -> Result<PathBuf> {
    if !is_safe_id(id) {
        bail!("invalid note id '{id}'");
    }
    Ok(Path::new(data_dir).join("notes").join(format!("{id}.json")))
}

/// The stored block array, or `None` when the host has no notes yet.
pub fn load_notes(data_dir: &str, id: &str) -> Option<Vec<serde_json::Value>> {
    let path = note_path(data_dir, id).ok()?;
    let s = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&s).ok()? {
        serde_json::Value::Array(a) => Some(a),
        _ => None,
    }
}

pub fn save_notes(data_dir: &str, id: &str, blocks: &[serde_json::Value]) -> Result<()> {
    let path = note_path(data_dir, id)?;
    let mut s = serde_json::to_string_pretty(blocks)?;
    s.push('\n');
    write_atomic(&path, s.as_bytes())
}

pub fn delete_notes(data_dir: &str, id: &str) {
    if let Ok(path) = note_path(data_dir, id) {
        let _ = std::fs::remove_file(path);
    }
}

// --- uploads ---------------------------------------------------------------

fn ext_for_image(content_type: &str) -> Option<&'static str> {
    Some(match content_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        _ => return None,
    })
}

fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

/// Save an uploaded image; returns the `/uploads/<name>` URL.
pub fn save_upload(data_dir: &str, content_type: &str, bytes: &[u8]) -> Result<String> {
    let Some(ext) = ext_for_image(content_type) else {
        bail!("unsupported image type '{content_type}'");
    };
    if bytes.len() > MAX_UPLOAD_BYTES {
        bail!("file too large (max 15 MB)");
    }
    let name = format!("{}.{ext}", rand_hex());
    write_atomic(&Path::new(data_dir).join("uploads").join(&name), bytes)?;
    Ok(format!("/uploads/{name}"))
}

/// Read an upload for serving. Only a generated `<hex>.<ext>` name is valid.
pub fn read_upload(data_dir: &str, name: &str) -> Result<(Vec<u8>, &'static str)> {
    let valid = name
        .split_once('.')
        .is_some_and(|(stem, ext)| {
            !stem.is_empty()
                && stem.bytes().all(|b| b.is_ascii_hexdigit())
                && !ext.is_empty()
                && ext.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        });
    if !valid {
        bail!("not found");
    }
    let ext = name.rsplit('.').next().unwrap_or("");
    let bytes = std::fs::read(Path::new(data_dir).join("uploads").join(name))?;
    Ok((bytes, mime_for_ext(ext)))
}

// --- detector feedback -----------------------------------------------------

/// The detector-feedback records directory name (`<data_dir>/detector-feedback`): where
/// `save_detector_feedback` writes and the `feedback` SMB share (`smb.rs`) reads. Single-sourced
/// here so the writer and the share path can never diverge — mirrors `homes::hosts_root`.
pub const DETECTOR_FEEDBACK_DIR: &str = "detector-feedback";

/// Lexical root of the detector-feedback records under `data_dir`. Pure (no symlink resolution),
/// so it's unit-testable; `smb.rs` wraps it with `std::path::absolute` for the share `path`.
pub fn detector_feedback_root(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join(DETECTOR_FEEDBACK_DIR)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectorFeedback {
    /// "false-positive" | "false-negative".
    pub kind: String,
    /// "screen" | "text" — which detector mode produced the verdict. Older
    /// clone-daemons don't send it; default to "screen".
    #[serde(default = "default_feedback_mode")]
    pub mode: String,
    /// "working" | "needs-human".
    pub detector_verdict: String,
    pub detector_reason: String,
    pub actual_state: String,
    #[serde(default)]
    pub ignore_reasons: Vec<String>,
    /// Text mode: the operator criteria the verdict was judged against.
    #[serde(default)]
    pub criteria: String,
    #[serde(default)]
    pub note: String,
}

fn default_feedback_mode() -> String {
    "screen".into()
}

/// Persist one feedback record + its artifact (screenshot in screen mode, the
/// captured pane text in text mode; either may be absent). Returns the record id.
pub fn save_detector_feedback(
    data_dir: &str,
    host_id: &str,
    fb: &DetectorFeedback,
    screenshot: Option<&[u8]>,
    capture: Option<&str>,
) -> Result<String> {
    let id = rand_hex();
    let dir = detector_feedback_root(data_dir);
    std::fs::create_dir_all(&dir)?;

    let mut image = String::new();
    if let Some(shot) = screenshot {
        if !shot.is_empty() {
            if shot.len() > MAX_UPLOAD_BYTES {
                bail!("screenshot too large (max 15 MB)");
            }
            image = format!("{id}.jpg");
            std::fs::write(dir.join(&image), shot)?;
        }
    }
    let mut capture_file = String::new();
    if let Some(text) = capture {
        if !text.is_empty() {
            if text.len() > MAX_UPLOAD_BYTES {
                bail!("capture too large (max 15 MB)");
            }
            capture_file = format!("{id}.txt");
            std::fs::write(dir.join(&capture_file), text)?;
        }
    }

    let record = serde_json::json!({
        "id": id, "host": host_id, "image": image, "capture": capture_file,
        "kind": fb.kind, "mode": fb.mode, "detectorVerdict": fb.detector_verdict,
        "detectorReason": fb.detector_reason, "actualState": fb.actual_state,
        "ignoreReasons": fb.ignore_reasons, "criteria": fb.criteria, "note": fb.note,
    });
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("index.jsonl"))?;
    writeln!(f, "{}", serde_json::to_string(&record)?)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_roundtrip_and_delete() {
        let dir = std::env::temp_dir().join(format!("rmng-notes-{}", rand_hex()));
        let d = dir.to_str().unwrap();
        assert!(load_notes(d, "h1").is_none());
        let blocks = vec![serde_json::json!({"type":"paragraph","text":"hi"})];
        save_notes(d, "h1", &blocks).unwrap();
        assert_eq!(load_notes(d, "h1").unwrap(), blocks);
        delete_notes(d, "h1");
        assert!(load_notes(d, "h1").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detector_feedback_persists_text_capture_and_mode() {
        let dir = std::env::temp_dir().join(format!("rmng-fb-{}", rand_hex()));
        let d = dir.to_str().unwrap();
        let fb = DetectorFeedback {
            kind: "false-positive".into(),
            mode: "text".into(),
            detector_verdict: "needs-human".into(),
            detector_reason: "idle input box".into(),
            actual_state: "working".into(),
            ignore_reasons: vec![],
            criteria: "STUCK if a dialog is shown".into(),
            note: "was mid-turn".into(),
        };
        let id = save_detector_feedback(d, "h1", &fb, None, Some("pane text here")).unwrap();
        // The capture landed as {id}.txt and the record carries mode/capture/criteria.
        let fb_dir = Path::new(d).join("detector-feedback");
        assert_eq!(std::fs::read_to_string(fb_dir.join(format!("{id}.txt"))).unwrap(), "pane text here");
        let index = std::fs::read_to_string(fb_dir.join("index.jsonl")).unwrap();
        let rec: serde_json::Value = serde_json::from_str(index.lines().last().unwrap()).unwrap();
        assert_eq!(rec["mode"], "text");
        assert_eq!(rec["capture"], format!("{id}.txt"));
        assert_eq!(rec["criteria"], "STUCK if a dialog is shown");
        assert_eq!(rec["image"], "");
        // Screen-mode record with neither artifact still saves (both optional).
        let fb2 = DetectorFeedback { mode: "screen".into(), criteria: String::new(), ..fb };
        let id2 = save_detector_feedback(d, "h1", &fb2, None, None).unwrap();
        let index = std::fs::read_to_string(fb_dir.join("index.jsonl")).unwrap();
        let rec2: serde_json::Value = serde_json::from_str(index.lines().last().unwrap()).unwrap();
        assert_eq!(rec2["id"], id2.as_str());
        assert_eq!(rec2["capture"], "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upload_rejects_nonimage_and_traversal() {
        let dir = std::env::temp_dir().join(format!("rmng-up-{}", rand_hex()));
        let d = dir.to_str().unwrap();
        assert!(save_upload(d, "text/plain", b"x").is_err());
        let url = save_upload(d, "image/png", b"\x89PNG").unwrap();
        let name = url.strip_prefix("/uploads/").unwrap();
        assert!(read_upload(d, name).is_ok());
        assert!(read_upload(d, "../../etc/passwd").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detector_feedback_root_joins_the_shared_dir_name() {
        // The on-disk folder name is wire-visible — docs and the `feedback` SMB share path
        // depend on it — so pin it: a rename must be deliberate, not accidental.
        assert_eq!(DETECTOR_FEEDBACK_DIR, "detector-feedback");
        assert_eq!(
            detector_feedback_root("data"),
            std::path::Path::new("data/detector-feedback")
        );
        assert_eq!(
            detector_feedback_root("/srv/rmng/data"),
            std::path::Path::new("/srv/rmng/data/detector-feedback")
        );
    }
}
