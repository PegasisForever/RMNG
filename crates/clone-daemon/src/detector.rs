//! The needs-human detector — `clone-daemon wait-for-stuck` + `report-detection`,
//! ported from the old `computer-use` crate (`wait_for_stuck.rs` + `detection.rs` +
//! `report.rs`). These run as short-lived **subcommands** (the agent arms
//! `wait-for-stuck` as a background task while it's working; on a needs-human verdict
//! it exits and the agent is re-engaged, then optionally `report-detection`s a wrong
//! verdict for tuning).
//!
//! Unlike the old binary, the detector does NOT create its own Mutter session: it
//! pulls screenshots from the **already-running daemon's MCP** over localhost, so
//! there's only ever one capture session per clone.
//!
//! It screenshots every interval, splits each monitor into independently-judged
//! cells, asks a local vision-LLM (OpenAI-compatible `/v1/chat/completions`) per
//! cell, and combines with "still-working wins". On a needs-human transition it
//! prints `desktop-state: needs-human — <reason>`; on `--timeout` it prints `timeout`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use futures::future::join_all;
use image::{DynamicImage, ImageFormat, RgbImage};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const DEFAULT_INFERENCE_URL: &str = "http://10.60.0.10:8080";
pub const DEFAULT_CONTROL_URL: &str = "http://10.60.0.1:9000";
const MODEL_MAX_H: u32 = 1080;
const TILE_COLS: u32 = 2;
const TILE_ROWS: u32 = 1;
const TILE_MAX_EDGE: u32 = 1200;

pub struct WaitOptions {
    pub inference_url: String,
    pub ignore_reasons: Vec<String>,
    pub interval: Duration,
    pub timeout: Duration,
    /// Port of the daemon's local MCP (screenshots come from there).
    pub mcp_port: u16,
}

pub struct ReportOptions {
    /// true = false-positive (said needs-human, was working); false = false-negative.
    pub false_positive: bool,
    pub note: String,
    pub control_url: String,
}

// --- last-detection slot (sidecar to report-detection) ----------------------

#[derive(Serialize, Deserialize, Clone)]
struct LastDetection {
    needs_human: bool,
    reason: String,
    #[serde(default)]
    ignore_reasons: Vec<String>,
}

fn base_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}
fn jpeg_path() -> PathBuf {
    base_dir().join("clone-daemon-last-detection.jpg")
}
fn meta_path() -> PathBuf {
    base_dir().join("clone-daemon-last-detection.json")
}

fn save_detection(jpeg: &[u8], meta: &LastDetection) -> Result<()> {
    std::fs::write(jpeg_path(), jpeg).context("writing last-detection jpeg")?;
    std::fs::write(meta_path(), serde_json::to_vec_pretty(meta)?).context("writing last-detection meta")?;
    Ok(())
}

fn load_detection() -> Result<(Vec<u8>, LastDetection)> {
    let jpeg = std::fs::read(jpeg_path())
        .with_context(|| format!("reading {} — run `wait-for-stuck` first", jpeg_path().display()))?;
    let meta: LastDetection =
        serde_json::from_slice(&std::fs::read(meta_path())?).context("parsing last-detection meta")?;
    Ok((jpeg, meta))
}

// --- wait-for-stuck ---------------------------------------------------------

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TileState {
    Working,
    NeedsHuman,
    Nothing,
}

#[derive(Deserialize)]
struct TileVerdict {
    state: TileState,
    #[serde(default)]
    reason: String,
}

pub async fn wait_for_stuck(opts: WaitOptions) -> Result<()> {
    let client = reqwest::Client::builder().build().context("building HTTP client")?;
    let base = opts.inference_url.trim_end_matches('/').to_string();
    let endpoint = format!("{base}/v1/chat/completions");
    let mcp = format!("http://127.0.0.1:{}/", opts.mcp_port);
    tracing::info!(
        "wait-for-stuck: polling {endpoint} every {}s for up to {}s ({} ignore-reason(s))",
        opts.interval.as_secs(),
        opts.timeout.as_secs(),
        opts.ignore_reasons.len(),
    );

    let deadline = Instant::now() + opts.timeout;
    loop {
        if Instant::now() >= deadline {
            println!("timeout");
            return Ok(());
        }
        match check_once(&client, &mcp, &endpoint, &opts.ignore_reasons).await {
            Ok(Some((needs_human, reason, jpeg))) => {
                let _ = save_detection(
                    &jpeg,
                    &LastDetection { needs_human, reason: reason.clone(), ignore_reasons: opts.ignore_reasons.clone() },
                );
                if needs_human {
                    let reason = reason.trim();
                    if reason.is_empty() {
                        println!("desktop-state: needs-human");
                    } else {
                        println!("desktop-state: needs-human — {reason}");
                    }
                    return Ok(());
                }
                tracing::info!("wait-for-stuck: still working ({reason})");
            }
            Ok(None) => tracing::info!("wait-for-stuck: no display / nothing to judge"),
            Err(e) => tracing::warn!("wait-for-stuck: check failed: {e:#}"),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::sleep(opts.interval.min(remaining)).await;
    }
}

/// One MCP `tools/call` to the local daemon; returns the `result.content` array.
async fn mcp_call(client: &reqwest::Client, url: &str, name: &str, args: Value) -> Result<Value> {
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } });
    let v: Value = client.post(url).json(&req).send().await?.error_for_status()?.json().await?;
    if let Some(err) = v.get("error") {
        bail!("MCP {name} error: {err}");
    }
    v.get("result").and_then(|r| r.get("content")).cloned().context("MCP result missing content")
}

/// Pull every monitor's screenshot (PNG) from the running daemon's MCP.
async fn screenshot_all(client: &reqwest::Client, mcp: &str) -> Result<Vec<RgbImage>> {
    let content = mcp_call(client, mcp, "list_monitors", json!({})).await.context("list_monitors")?;
    let listing = content.get(0).and_then(|c| c.get("text")).and_then(Value::as_str).context("list_monitors text")?;
    let mons: Vec<Value> = serde_json::from_str(listing).context("parsing monitor list")?;
    let ids: Vec<u64> = mons.iter().filter_map(|m| m.get("id").and_then(Value::as_u64)).collect();

    let mut imgs = Vec::with_capacity(ids.len());
    for id in ids {
        let content = mcp_call(client, mcp, "screenshot", json!({ "monitor": id })).await?;
        let b64 = content
            .get(0)
            .and_then(|c| c.get("data"))
            .and_then(Value::as_str)
            .context("screenshot content missing data")?;
        let png = B64.decode(b64).context("decoding screenshot base64")?;
        let mut img = image::load_from_memory(&png).context("decoding screenshot PNG")?;
        if img.height() > MODEL_MAX_H {
            let w = (u64::from(img.width()) * u64::from(MODEL_MAX_H) / u64::from(img.height().max(1))).max(1) as u32;
            img = img.resize_exact(w, MODEL_MAX_H, image::imageops::FilterType::Triangle);
        }
        imgs.push(img.to_rgb8());
    }
    Ok(imgs)
}

async fn check_once(
    client: &reqwest::Client,
    mcp: &str,
    endpoint: &str,
    ignore_reasons: &[String],
) -> Result<Option<(bool, String, Vec<u8>)>> {
    let monitors = screenshot_all(client, mcp).await?;
    if monitors.is_empty() {
        return Ok(None);
    }
    let composite = compose(&monitors)?;
    let cells = tile_cells(&monitors)?;
    let verdicts: Vec<(String, Result<TileVerdict>)> = join_all(cells.iter().map(|cell| async move {
        let data_url = format!("data:image/jpeg;base64,{}", B64.encode(&cell.jpeg));
        (cell.label.clone(), judge_cell(client, endpoint, &data_url, ignore_reasons).await)
    }))
    .await;

    let mut ok = Vec::new();
    let mut errs = 0;
    for (label, r) in verdicts {
        match r {
            Ok(v) => ok.push(v),
            Err(e) => {
                errs += 1;
                tracing::debug!("cell {label} judgement failed: {e:#}");
            }
        }
    }
    if ok.is_empty() {
        bail!("all {errs} cell judgement(s) failed");
    }
    let (needs_human, reason) = combine(&ok);
    Ok(Some((needs_human, reason, composite)))
}

/// "Still-working wins": any working cell ⇒ still working; else trust a needs-human cell.
fn combine(verdicts: &[TileVerdict]) -> (bool, String) {
    if let Some(v) = verdicts.iter().find(|v| v.state == TileState::Working) {
        let r = v.reason.trim();
        return (false, if r.is_empty() { "still working".into() } else { format!("still working: {r}") });
    }
    if let Some(v) = verdicts
        .iter()
        .find(|v| v.state == TileState::NeedsHuman && !v.reason.trim().is_empty())
        .or_else(|| verdicts.iter().find(|v| v.state == TileState::NeedsHuman))
    {
        return (true, v.reason.clone());
    }
    (false, "nothing on screen yet".into())
}

const SYSTEM_PROMPT: &str = "\
You are a vigilant monitor watching a Linux desktop that is running an automated \
task — typically an AI coding assistant such as Claude Code inside an editor like \
Cursor or VS Code. You are shown a CROP of part of the screen. Judge ONLY what is \
visible in this crop and classify it as exactly one of: working, needs_human, nothing.

WORKING — something in this crop is actively making progress ON ITS OWN right now. \
Concrete cues, any one is enough:
- An AI agent is running: a red or orange filled SQUARE stop button where a send \
button would be (the single most reliable cue); a live spinner or animated status \
word (\"Vibing\", \"Implementing\", \"Coalescing\", \"Garnishing\", \"Pondering\", \
\"Musing\", \"Sussing\", \"Deciphering\", \"Working\", \"Thinking\", \"Generating\", \
\"Running … command\"); a ticking elapsed-time or token counter; the text \"esc to \
interrupt\"; or a chat input whose placeholder reads \"Queue another message\" (an \
editor shows this ONLY while its agent is running, because you queue a message to \
run after the current one).
- A page is loading (spinner / progress bar / half-rendered), or a build, compile, \
test, install, download or upload is in progress / printing output.
The LIVE status line and the send button win over the transcript: a log full of \
finished steps, \"Done\", checkmarks or past-tense summaries above still counts as \
WORKING if the bottom shows a stop button, a spinner, a ticking counter, or \"Queue \
another message\".

NEEDS_HUMAN — there is NO active progress in this crop AND a person is required to \
move forward:
- A coding assistant that FINISHED and sits at an idle prompt: the send button is a \
plain triangular ARROW (not a stop square), there is no spinner/timer, and the \
placeholder invites a NEW task (e.g. \"Write a message\", \"Plan, search, build \
anything\", \"ctrl esc to focus or unfocus\"); or it posted a final summary / \
\"ready whenever you give the go-ahead\".
- A dialog awaiting a decision (save/discard, authentication, permission, \
confirmation), a question awaiting an answer, an error/crash blocking progress, or \
a paused installer/wizard.

NOTHING — this crop has no relevant signal: blank wallpaper, an empty area, or just \
a file tree / code with no agent activity and no waiting dialog.

Respond ONLY with JSON matching {\"state\": \"working\"|\"needs_human\"|\"nothing\", \
\"reason\": string}. reason is short and specific — what is in progress, or what a \
human is being waited on for.";

fn build_request(data_url: &str, ignore_reasons: &[String]) -> Value {
    let mut user_text = String::from("Here is a crop of part of the desktop. Classify what is visible in it.");
    if !ignore_reasons.is_empty() {
        user_text.push_str(
            "\n\nIMPORTANT: treat the following situations as WORKING (state=\"working\") — they do \
             NOT need a human even if they look idle or finished:",
        );
        for r in ignore_reasons {
            user_text.push_str("\n- ");
            user_text.push_str(r);
        }
    }
    json!({
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": [
                { "type": "text", "text": user_text },
                { "type": "image_url", "image_url": { "url": data_url } }
            ]}
        ],
        "temperature": 0,
        "max_tokens": 256,
        "chat_template_kwargs": { "enable_thinking": false },
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "tile_verdict", "strict": true,
                "schema": {
                    "type": "object",
                    "properties": { "state": { "type": "string", "enum": ["working", "needs_human", "nothing"] }, "reason": { "type": "string" } },
                    "required": ["state", "reason"], "additionalProperties": false
                }
            }
        }
    })
}

async fn judge_cell(client: &reqwest::Client, endpoint: &str, data_url: &str, ignore_reasons: &[String]) -> Result<TileVerdict> {
    let body = build_request(data_url, ignore_reasons);
    let resp = client.post(endpoint).json(&body).send().await?.error_for_status()?;
    let v: Value = resp.json().await.context("decoding inference response")?;
    let content = v["choices"][0]["message"]["content"].as_str().context("response missing content")?;
    parse_verdict(content)
}

fn parse_verdict(content: &str) -> Result<TileVerdict> {
    if let Ok(v) = serde_json::from_str::<TileVerdict>(content) {
        return Ok(v);
    }
    if let (Some(a), Some(b)) = (content.find('{'), content.rfind('}')) {
        if a < b {
            return serde_json::from_str::<TileVerdict>(&content[a..=b]).context("no parseable JSON in model output");
        }
    }
    bail!("model output was not JSON: {content:?}")
}

struct Cell {
    label: String,
    jpeg: Vec<u8>,
}

/// Stack monitors top-to-bottom into one JPEG (the frame saved for report-detection).
fn compose(monitors: &[RgbImage]) -> Result<Vec<u8>> {
    let max_w = monitors.iter().map(|im| im.width()).max().unwrap_or(1).max(1);
    let total_h = monitors.iter().map(|im| im.height()).sum::<u32>().max(1);
    let mut canvas = RgbImage::from_pixel(max_w, total_h, image::Rgb([0, 0, 0]));
    let mut y_off: i64 = 0;
    for im in monitors {
        image::imageops::overlay(&mut canvas, im, 0, y_off);
        y_off += i64::from(im.height());
    }
    let mut buf = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(canvas).write_to(&mut buf, ImageFormat::Jpeg).context("JPEG composite")?;
    Ok(buf.into_inner())
}

/// Split each monitor into TILE_COLS×TILE_ROWS cells, cap the long edge, JPEG-encode.
fn tile_cells(monitors: &[RgbImage]) -> Result<Vec<Cell>> {
    use image::imageops::{FilterType, crop_imm, resize};
    let mut cells = Vec::with_capacity(monitors.len() * (TILE_COLS * TILE_ROWS) as usize);
    for (mon_idx, img) in monitors.iter().enumerate() {
        let (w, h) = (img.width(), img.height());
        for r in 0..TILE_ROWS {
            for c in 0..TILE_COLS {
                let (x0, x1) = (w * c / TILE_COLS, w * (c + 1) / TILE_COLS);
                let (y0, y1) = (h * r / TILE_ROWS, h * (r + 1) / TILE_ROWS);
                let (cw, ch) = (x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1));
                let mut cell: RgbImage = crop_imm(img, x0, y0, cw, ch).to_image();
                let long = cw.max(ch);
                if long > TILE_MAX_EDGE {
                    let scale = f64::from(TILE_MAX_EDGE) / f64::from(long);
                    let nw = ((f64::from(cw) * scale).round() as u32).max(1);
                    let nh = ((f64::from(ch) * scale).round() as u32).max(1);
                    cell = resize(&cell, nw, nh, FilterType::Triangle);
                }
                let mut buf = std::io::Cursor::new(Vec::new());
                DynamicImage::ImageRgb8(cell).write_to(&mut buf, ImageFormat::Jpeg).context("JPEG cell")?;
                cells.push(Cell { label: format!("mon{mon_idx} r{r}c{c}"), jpeg: buf.into_inner() });
            }
        }
    }
    Ok(cells)
}

// --- report-detection -------------------------------------------------------

pub async fn report_detection(opts: ReportOptions) -> Result<()> {
    let (jpeg, meta) = load_detection()?;
    let detector_verdict = if meta.needs_human { "needs-human" } else { "working" };
    let (kind, actual_state) =
        if opts.false_positive { ("false-positive", "working") } else { ("false-negative", "needs-human") };
    if opts.false_positive != meta.needs_human {
        tracing::warn!("reported {kind} but the last verdict was {detector_verdict}; reporting anyway");
    }

    let base = opts.control_url.trim_end_matches('/');
    let endpoint = format!("{base}/api/detector-feedback");
    let mut form = reqwest::multipart::Form::new()
        .text("kind", kind)
        .text("detectorVerdict", detector_verdict)
        .text("detectorReason", meta.reason.clone())
        .text("actualState", actual_state)
        .text("note", opts.note.clone());
    for r in &meta.ignore_reasons {
        form = form.text("ignoreReason", r.clone());
    }
    let part = reqwest::multipart::Part::bytes(jpeg)
        .file_name("screenshot.jpg")
        .mime_str("image/jpeg")
        .context("building screenshot part")?;
    form = form.part("screenshot", part);

    let client = reqwest::Client::builder().build().context("building HTTP client")?;
    let resp = client.post(&endpoint).multipart(form).send().await.with_context(|| format!("posting to {endpoint}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("control server returned {status}: {body}");
    }
    println!("reported {kind} to the control server ({})", body.trim());
    Ok(())
}
